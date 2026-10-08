//! The restart runner: effect ordering, revival, and the phases that license a mint or a spawn.

use super::super::support_tests::*;
use super::*;

/// The four effects, named so the ORDER they happened in is a value a
/// test can compare rather than a shape a reader has to infer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    Status,
    EndRun,
    Spawn,
    Reset,
}

/// The four effects, recorded in the order `restart_blocking` performs
/// them, with each answer scripted. Shared state behind `Arc<Mutex<..>>`
/// like `Peer`'s bytes, for the same reason: the thing under test is a
/// call that may be ABSENT, and an absence has no return value to
/// assert on — only the record shows it.
struct FakeSupervisor {
    record: std::sync::Arc<std::sync::Mutex<Vec<Effect>>>,
    /// The argv the replacement was actually handed, so one test can
    /// pin that it spends `--resume <id>` rather than `--continue`.
    spawned_argv: std::sync::Arc<std::sync::Mutex<Option<Vec<String>>>>,
    /// The two status answers, drained in order: the pre-retire
    /// identity read, then the post-spawn settle read. Both are phases
    /// `phase_rests` accepts, so the wait returns on its first poll and
    /// no test ever sleeps on the real 30 s deadline.
    status: std::sync::Mutex<std::collections::VecDeque<(u32, u64, sot_log::lane::wire::SupervisorPhase)>>,
    end_run: Result<crate::rows::run::end_run::EndRunOutcome, String>,
    spawn: Result<&'static str, String>,
    reset: Result<String, String>,
}

impl FakeSupervisor {
    /// The healthy revival: an authority at `(1111, 800)` before the
    /// retire, a different one at `(4242, 900)` after it, both resting
    /// where a resumed run rests, an end that verified and a reset that
    /// minted.
    fn healthy() -> Self {
        use sot_log::lane::wire::SupervisorPhase as P;
        Self {
            record: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            spawned_argv: std::sync::Arc::new(std::sync::Mutex::new(None)),
            status: std::sync::Mutex::new(
                [(1111, 800, P::EndedNoRespawn), (4242, 900, P::EndedNoRespawn)].into_iter().collect(),
            ),
            end_run: Ok(crate::rows::run::end_run::EndRunOutcome::RecordVerified),
            spawn: Ok("starting"),
            reset: Ok("voyage-1".to_string()),
        }
    }

    /// Re-script the two reads: the identity read, then the settle read.
    fn status_reads(
        &self,
        identity: (u32, u64, sot_log::lane::wire::SupervisorPhase),
        settled: (u32, u64, sot_log::lane::wire::SupervisorPhase),
    ) {
        *self.status.lock().unwrap() = [identity, settled].into_iter().collect();
    }

    fn record(&self) -> Vec<Effect> {
        self.record.lock().unwrap().clone()
    }

    fn spawned_argv(&self) -> Vec<String> {
        self.spawned_argv.lock().unwrap().clone().expect("the replacement was spawned")
    }
}

impl RestartEffects for FakeSupervisor {
    fn query_status(&self, _state_dir: &Path) -> Result<sot_log::attach_client::supervisor_client::StatusReport, String> {
        self.record.lock().unwrap().push(Effect::Status);
        let (pid, created, phase) = self
            .status
            .lock()
            .unwrap()
            .pop_front()
            .expect("the fake was asked for more status reads than it was scripted");
        Ok(sot_log::attach_client::supervisor_client::StatusReport { pid, created, voyage: None, leg: None, phase })
    }
    fn end_run(
        &self,
        _state_dir: &Path,
        _reason: &str,
        _root_canonicalized: bool,
    ) -> std::io::Result<crate::rows::run::end_run::EndRunOutcome> {
        self.record.lock().unwrap().push(Effect::EndRun);
        self.end_run.clone().map_err(std::io::Error::other)
    }
    fn spawn_replacement(&self, plan: &ReauthRestart) -> Result<&'static str, String> {
        self.record.lock().unwrap().push(Effect::Spawn);
        *self.spawned_argv.lock().unwrap() = Some(plan.argv.clone());
        self.spawn.clone()
    }
    fn reset(&self, _workspaces: &Workspaces, _workspace_id: &str, _state_dir: &Path) -> Result<String, String> {
        self.record.lock().unwrap().push(Effect::Reset);
        self.reset.clone()
    }
}

// ORDERING 5: a revival is THREE effects and this is the one test that
// can see all three happen, in order, at all — the defect it exists for
// is a reset that is never called, and an absent call has no return
// value for a pure function to observe. The single sequence assertion
// below pins every ordering this path depends on: the identity is read
// BEFORE the retire (so a leaked retire is detectable at all), the
// spawn follows the end, and the mint follows a settle read that
// follows the spawn.
#[tokio::test]
async fn a_revival_is_the_identity_then_end_run_then_spawn_then_the_settle_then_the_mint() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, _slug) = accept_fixture(home.path(), scratch.path());

    let (payload, restart) = reauth(&reg, &id, "team", &sid(7)).await;
    assert_eq!(payload["code"], ACCEPTED_CODE);
    let fake = FakeSupervisor::healthy();
    restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

    assert_eq!(
        fake.record(),
        vec![Effect::Status, Effect::EndRun, Effect::Spawn, Effect::Status, Effect::Reset]
    );
    assert_eq!(
        reg.resolve(Some(&id)).unwrap().account(),
        "team",
        "a revival that completed never rolls the record back"
    );
    // The one call that still holds the conversation id spends it: the
    // id is never persisted on the row, so a later attach's `--continue`
    // would select by recency instead.
    let argv = fake.spawned_argv();
    assert!(argv.iter().any(|a| a == "--resume"), "{argv:?}");
    assert!(argv.iter().any(|a| *a == sid(7)), "{argv:?}");
    assert!(!argv.iter().any(|a| a == "--continue"), "{argv:?}");
}

// An end that could not run leaves the OLD leg on the OLD login, so
// nothing may be spawned and the record has to go back to saying so.
#[tokio::test]
async fn an_end_run_that_cannot_run_spawns_nothing_and_rolls_the_record_back() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

    let (_payload, restart) = reauth(&reg, &id, "team", &sid(7)).await;
    let mut fake = FakeSupervisor::healthy();
    fake.end_run = Err("the lane never answered".to_string());
    restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

    assert_eq!(fake.record(), vec![Effect::Status, Effect::EndRun]);
    assert_eq!(
        reg.resolve(Some(&id)).unwrap().account(),
        "",
        "the record is back on the login the live leg still spends"
    );
    let toml = std::fs::read_to_string(crate::rows::store::toml_path_for(&slug)).unwrap();
    assert!(toml.contains("account       = \"\""), "{toml}");
}

// `end_run` can SUCCEED and still leave the run unended: `NotEnded` means
// the authority did not end the run. Nothing
// else pins that `restart_blocking` consults that judgement — the fake's
// only scripted outcome is the healthy one, so without this the guard at
// `run_ended` could be deleted with every other test still green. An
// absent call is exactly what no pure test of `run_ended` can observe.
#[tokio::test]
async fn an_end_run_that_did_not_end_the_run_spawns_nothing_and_rolls_the_record_back() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

    let (_payload, restart) = reauth(&reg, &id, "team", &sid(7)).await;
    let mut fake = FakeSupervisor::healthy();
    fake.end_run = Ok(crate::rows::run::end_run::EndRunOutcome::NotEnded("the authority is still starting".to_string()));
    restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

    assert_eq!(
        fake.record(),
        vec![Effect::Status, Effect::EndRun],
        "the run did not end, so nothing may be spawned or minted"
    );
    assert_eq!(
        reg.resolve(Some(&id)).unwrap().account(),
        "",
        "the record is back on the login the live leg still spends"
    );
    let toml = std::fs::read_to_string(crate::rows::store::toml_path_for(&slug)).unwrap();
    assert!(toml.contains("account       = \"\""), "{toml}");
}

// The asymmetry `restart_blocking`'s own doc states and nothing tested:
// once the leg IS ended the record stands, whatever the spawn does,
// because every later start path reads the account off the registry. A
// rollback here would point the next attach at the login whose leg no
// longer exists.
#[tokio::test]
async fn a_spawn_that_fails_leaves_the_record_on_the_new_account() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

    let (_payload, restart) = reauth(&reg, &id, "team", &sid(7)).await;
    let mut fake = FakeSupervisor::healthy();
    fake.spawn = Err("the capsule binary is missing".to_string());
    restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

    assert_eq!(fake.record(), vec![Effect::Status, Effect::EndRun, Effect::Spawn]);
    assert_eq!(
        reg.resolve(Some(&id)).unwrap().account(),
        "team",
        "the leg is ended; the record must name the login the next start will spend"
    );
    let toml = std::fs::read_to_string(crate::rows::store::toml_path_for(&slug)).unwrap();
    assert!(toml.contains("account       = \"team\""), "{toml}");
}

// `ready` means a leg is ALREADY live — an `end_run` whose stop ended
// the authority but not the leg — so the row has a leg on the OLD
// login. The path must consult the judgement and STOP, which is a
// missing `Reset` no pure test of `ready_to_mint` can observe.
#[tokio::test]
async fn a_replacement_that_rests_with_a_leg_live_is_never_minted_on() {
    use sot_log::lane::wire::SupervisorPhase as P;
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, _slug) = accept_fixture(home.path(), scratch.path());

    let (_payload, restart) = reauth(&reg, &id, "team", &sid(7)).await;
    let fake = FakeSupervisor::healthy();
    fake.status_reads((1111, 800, P::EndedNoRespawn), (4242, 900, P::Ready));
    restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

    assert_eq!(
        fake.record(),
        vec![Effect::Status, Effect::EndRun, Effect::Spawn, Effect::Status],
        "a live leg is never minted on"
    );
    assert_eq!(reg.resolve(Some(&id)).unwrap().account(), "team");
}

// The leaked retire driven through the WHOLE path: the same process
// answers before and after, resting at exactly the phase a healthy
// replacement rests at. Only the identity tells them apart, and this is
// what makes the identity read's POSITION load-bearing rather than
// incidental — read after the retire, there would be nothing to compare.
#[tokio::test]
async fn a_leaked_retire_is_never_minted_on_end_to_end() {
    use sot_log::lane::wire::SupervisorPhase as P;
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, _slug) = accept_fixture(home.path(), scratch.path());

    let (_payload, restart) = reauth(&reg, &id, "team", &sid(7)).await;
    let fake = FakeSupervisor::healthy();
    fake.status_reads((1111, 800, P::EndedNoRespawn), (1111, 800, P::EndedNoRespawn));
    restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

    assert_eq!(
        fake.record(),
        vec![Effect::Status, Effect::EndRun, Effect::Spawn, Effect::Status],
        "the process the switch was supposed to retire is never minted on"
    );
    assert_eq!(reg.resolve(Some(&id)).unwrap().account(), "team");
}

/// The defect this whole change exists for, and the one the review of
/// its first version found. A spawn alone was reported as a running
/// leg; the first fix then minted on ANY phase, merely warning when it
/// was unexpected. `Reset` is admissible only from `ended_no_respawn`,
/// so every other phase is a mint that would be refused -- and `ready`
/// is worse than refused, because it means a leg is already live on the
/// login the switch moved away from.
#[test]
fn only_the_resting_phase_of_a_resumed_run_licenses_a_mint() {
    use sot_log::lane::wire::SupervisorPhase as P;
    let fresh = (4242u32, 900u64);
    let retired = Some((1111u32, 800u64));
    assert!(super::ready_to_mint(P::EndedNoRespawn, retired, fresh).is_ok());
    for refused in [P::Ready, P::Terminal, P::Starting, P::Ending] {
        assert!(
            super::ready_to_mint(refused, retired, fresh).is_err(),
            "{refused:?} must never be minted on"
        );
    }
}

/// `ready` is named separately because its failure is not "nothing came
/// up" but "something did, on the wrong account" -- the message has to
/// say so, or the next reader chases a missing leg that is running.
#[test]
fn a_live_leg_is_reported_as_the_old_login_not_as_an_absence() {
    use sot_log::lane::wire::SupervisorPhase as P;
    let detail = super::ready_to_mint(P::Ready, Some((1111, 800)), (4242, 900))
        .expect_err("a live leg is not a licence to mint");
    assert!(detail.contains("already live"), "got {detail:?}");
    assert!(detail.contains("moved away from"), "got {detail:?}");
}

/// `end_run`'s stop is best effort. When it leaks, the OLD supervisor
/// is still resident, this call's own spawn exits at the authority
/// fence, and the status read comes back from that old process resting
/// at exactly the phase a healthy replacement rests at. Only its
/// identity tells the two apart, and minting on it would spawn the leg
/// from its own cached argv and account.
#[test]
fn a_leaked_retire_is_never_minted_on_however_it_rests() {
    use sot_log::lane::wire::SupervisorPhase as P;
    let same = (1111u32, 800u64);
    let detail = super::ready_to_mint(P::EndedNoRespawn, Some(same), same)
        .expect_err("the process the switch retired is not a replacement");
    assert!(detail.contains("retire leaked"), "the failure must name the cause, got {detail:?}");
    assert!(detail.contains("1111"), "the failure must name the process, got {detail:?}");
}

/// A pid that merely REPEATS is not the same authority: the supervisor
/// reports its own creation stamp alongside, and the pair is what is
/// compared, so a recycled pid on a genuinely new process still mints.
#[test]
fn a_recycled_pid_on_a_new_authority_still_mints() {
    use sot_log::lane::wire::SupervisorPhase as P;
    assert!(super::ready_to_mint(P::EndedNoRespawn, Some((1111, 800)), (1111, 901)).is_ok());
}

/// Nothing answering before the retire is the ordinary case for a row
/// whose authority had already gone; it is not evidence of a leak.
#[test]
fn an_unknown_predecessor_does_not_block_the_mint() {
    use sot_log::lane::wire::SupervisorPhase as P;
    assert!(super::ready_to_mint(P::EndedNoRespawn, None, (4242, 900)).is_ok());
}

/// The wait's partition is the supervisor's own: these three are where
/// an authority stays until somebody acts, and the rest are phases it
/// is still moving through -- which is the whole reason the previous
/// version's two-second settle was not enough to believe.
#[test]
fn only_settled_phases_end_the_wait() {
    use sot_log::lane::wire::SupervisorPhase as P;
    for resting in [P::Ready, P::EndedNoRespawn, P::Terminal] {
        assert!(super::phase_rests(resting), "{resting:?} rests");
    }
    for moving in [P::Starting, P::Ending] {
        assert!(!super::phase_rests(moving), "{moving:?} is still moving");
    }
}

// `end_run`'s outcomes partition into "the run is over" (spawn the
// replacement) and "something still holds this row" (leave it alone) —
// the same partition `capsule_destroy_outcome_of` makes for removal.
#[test]
fn only_an_ended_run_licenses_a_replacement_spawn() {
    use crate::rows::run::end_run::EndRunOutcome as O;
    for over in [O::RecordVerified, O::RecordClosed, O::AlreadyEnded, O::Terminal, O::Unheld, O::Orphaned] {
        assert!(run_ended(&over).is_ok(), "{over:?}");
    }
    assert!(run_ended(&O::NotEnded("starting".to_string())).is_err());
    match run_ended(&O::NotEnded("a leg is running".to_string())) {
        Err(detail) => assert_eq!(detail, "a leg is running"),
        Ok(()) => panic!("a run that did not end must never license a spawn"),
    }
}
