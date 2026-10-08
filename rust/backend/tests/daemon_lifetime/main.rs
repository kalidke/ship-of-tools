#![cfg(unix)]
#![cfg_attr(not(target_os = "linux"), allow(dead_code, reason = "the successor case, which uses the barrier folder and the saved results, is Linux only until the macOS authority exists"))]
//! The daemon-lifetime harness: the premises and, from the next commits, the cases of lane L2 (a daemon lifetime and
//! the children it owns), run on real processes. Nothing here stands in for a daemon, and nothing is proved by
//! reading source text: a premise is a real launch, a real lock or a real `sotd` and `sot-capsule`, and every death
//! is read from an identity the fixture opened while the process was alive (`fixture_owner`, `native`).
//!
//! The cases that hold a real process at a phase barrier (`successor`, the pause cases of `native_premises`) need
//! the barrier build: `cargo build -p sot-log --features native-barrier --bin sot-capsule` into the target the
//! tests run from, then `cargo test -p sot-backend --features daemon-lifetime-faults --test daemon_lifetime`.

mod fixture_owner;
mod native;
mod native_premises;
mod observations;
#[cfg(target_os = "linux")]
mod successor;
#[cfg(target_os = "linux")]
#[allow(
    dead_code,
    reason = "the shared fixture serves more suites than this one uses"
)]
#[path = "../support/mod.rs"]
mod support;
mod workers;
