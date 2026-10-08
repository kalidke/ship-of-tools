#![cfg(unix)]
#![cfg_attr(
    not(all(target_os = "linux", feature = "daemon-lifetime-faults")),
    allow(
        dead_code,
        reason = "the successor case, which uses the barrier folder and the saved results, is built only with the barrier build, on Linux until the macOS authority exists"
    )
)]
//! The daemon-lifetime harness: the premises and, from the next commits, the cases of lane L2 (a daemon lifetime and
//! the children it owns), run on real processes. Nothing here stands in for a daemon, and nothing is proved by
//! reading source text: a premise is a real launch, a real lock or a real `sotd` and `sot-capsule`, and every death
//! is read from an identity the fixture opened while the process was alive (`fixture_owner`, `native`).
//!
//! The cases that hold a real process at a phase barrier (`successor`, the pause cases of `native_premises`) need
//! the barrier build: `cargo build -p sot-log --features native-barrier --bin sot-capsule` into the target the
//! tests run from, then `cargo test -p sot-backend --features daemon-lifetime-faults --test daemon_lifetime`.

#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
mod done;
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
mod durable;
mod fixture_owner;
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
mod guard;
mod native;
mod native_premises;
mod observations;
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
mod routes;
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
mod successor;
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
#[allow(
    dead_code,
    reason = "the shared fixture serves more suites than this one uses"
)]
#[path = "../support/mod.rs"]
mod support;
mod workers;

/// The cases that start a daemon take this first: `Env::new` points this process's `SOT_RUNTIME_DIR` at its own folder.
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
pub static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Write an executable stub `systemctl` into `dir` (the relay-refresh case's stand-in).
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
pub fn native_stub_systemctl(dir: &std::path::Path, body: &str) {
    sot_log::test_exec::write_executable(&dir.join("systemctl"), body);
}

#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
#[path = "fixtures/tree.rs"]
mod tree;
