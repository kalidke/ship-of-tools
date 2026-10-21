use super::*;
use sot_updater::identity::DEFAULT_REPO;

fn topo(text: &str) -> sot_protocol::topology::Topology {
    sot_protocol::topology::parse(text).expect("fixture must parse")
}

// The regression this pins: `install.role != "remote"` used to decide
// this; install.json carries no role any more (plan step 6), so a
// daemon box and a frontend-only box must still take the branch they
// always did, now read from the declared topology first, install.json's
// recorded `daemon` bit second (a listless box), `true` last.

fn an_identity() -> sot_updater::ReleaseIdentity {
    sot_updater::ReleaseIdentity {
        repo: DEFAULT_REPO.to_string(),
        tag: "v9.9.9".into(),
        version: "9.9.9".into(),
        target: "x86_64-unknown-linux-musl".into(),
        asset: "sot-9.9.9-x86_64-unknown-linux-musl.tar.gz".into(),
        asset_sha256: "0".repeat(64),
    }
}

// What `--update-status` tells the one person who can reach a
// frontend-only box. An interrupted stage must read as progress (bytes on
// disk, and the next start continuing from them), never as a reason to
// converge again — converging again is what four cycles did.
#[test]
fn an_interrupted_stage_reads_as_progress_not_as_a_reason_to_converge() {
    let at = Phase {
        partial_bytes: Some(44_040_192),
        staged: false,
        prepared: false,
        armed: false,
    };
    let lines = phase_lines(&an_identity(), &at).join("\n");
    assert!(lines.contains("downloaded     42.0 MiB of sot-9.9.9-"), "{lines}");
    assert!(lines.contains("staged         no"), "{lines}");
    assert!(lines.contains("next           the next frontend start continues"), "{lines}");
}

// Armed is the end of this box's own work: a converge applies it
// (`relaunch-sot.ps1`'s Invoke-PendingApply), so the instruction must not
// send anyone looking for a different ritual.
#[test]
fn an_armed_box_is_told_a_converge_applies_it() {
    let at = Phase {
        partial_bytes: None,
        staged: true,
        prepared: true,
        armed: true,
    };
    let lines = phase_lines(&an_identity(), &at).join("\n");
    assert!(lines.contains("downloaded     nothing yet"), "{lines}");
    assert!(lines.contains("armed          yes"), "{lines}");
    assert!(
        lines.contains("next           nothing — the next converge or launcher start applies it"),
        "{lines}"
    );
}

#[test]
fn a_daemon_box_leaves_updates_to_the_backend() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.host-2]\n\
             daemon = true\n",
    );
    assert!(backend_owns_updates(Some(&t), "host-2", None));
}

#[test]
fn a_frontend_entry_leaves_updates_to_its_own_daemon() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n\
             frontend = true\n",
    );
    assert!(backend_owns_updates(Some(&t), "laptop", None));
    assert!(backend_owns_updates(Some(&t), "laptop", Some(true)));
}

// An install from before the installer gave a frontend entry a daemon records `daemon: false` and runs none: its
// window is the only updater there.
#[test]
fn a_frontend_entry_whose_install_runs_no_daemon_self_updates() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n\
             frontend = true\n",
    );
    assert!(!backend_owns_updates(Some(&t), "laptop", Some(false)));
}

#[test]
fn a_box_the_topology_does_not_name_falls_back_to_the_recorded_bit() {
    let t = topo("hub = \"hubbox\"\n[host.hubbox]\ndaemon = true\n");
    assert!(backend_owns_updates(Some(&t), "nowhere", None));
}

#[test]
fn no_topology_and_no_recorded_bit_defaults_to_the_backend_owning_it() {
    assert!(backend_owns_updates(None, "anything", None));
}

// The case this closes: a listless frontend-only-over-ssh install has
// no hosts.toml at all, so nothing but install.json's own recorded
// `daemon: false` can tell the FE it owns its own updates here.
#[test]
fn a_listless_frontend_only_box_self_updates_per_its_recorded_bit() {
    assert!(!backend_owns_updates(None, "laptop", Some(false)));
}

#[test]
fn a_listless_daemon_box_leaves_updates_to_the_backend_per_its_recorded_bit() {
    assert!(backend_owns_updates(None, "host-2", Some(true)));
}

// The declared topology is canonical and can change without a
// reinstall — a stale recorded bit from install time must never win
// over what the list says NOW.
#[test]
fn the_declared_topology_wins_over_a_stale_recorded_bit() {
    let t = topo(
        "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n",
    );
    // install.json still says "daemon: true" from before this host was
    // listed with neither key.
    assert!(!backend_owns_updates(Some(&t), "laptop", Some(true)));
}
