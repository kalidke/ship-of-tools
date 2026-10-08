//! Storage exhaustion on a real bounded volume: what a full volume does to the capsule's callers.
#![cfg(any(target_os = "linux", target_os = "macos", windows))]

mod boundaries;
mod exits;
#[allow(dead_code)]
#[path = "../support/transports.rs"]
mod transports;
mod volume;

use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", windows))]
use std::process::{Command, Stdio};
use std::time::Duration;

fn scratch() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"));
    let target = if target.is_absolute() {
        target
    } else {
        std::env::current_dir().unwrap().join(target)
    };
    let scratch = target.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    scratch
}

#[cfg(any(target_os = "macos", windows))]
fn bounded(mut command: Command, seconds: u64) -> (i32, String) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("start the owned premise command");
    let (status, out, err) =
        sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(seconds));
    (status.code().unwrap_or(-1), format!("{out}{err}"))
}

#[cfg(any(target_os = "macos", windows))]
fn scrub(text: &str) -> String {
    let mut text = text.to_string();
    for (value, replacement) in [
        (std::env::var_os("CARGO_TARGET_DIR"), "<target>"),
        (
            Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .canonicalize()
                    .expect("canonical checkout root")
                    .into_os_string(),
            ),
            "<repo>",
        ),
        (std::env::var_os("HOME"), "<home>"),
        (std::env::var_os("USERPROFILE"), "<home>"),
        (Some(scratch().into_os_string()), "<scratch>"),
    ] {
        if let Some(value) = value {
            let value = value.to_string_lossy();
            if !value.is_empty() {
                text = text.replace(value.as_ref(), replacement);
            }
        }
    }
    text
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs the bounded ext4 volume of rust.yml's L3 step"
)]
fn native_volume_fill_free_sync() {
    #[cfg(windows)]
    volume::windows_unwind_control();
    volume::on_volume(|root| {
        sot_log::host::preflight_volume(root).expect("a native volume must pass capsule preflight");
        let code = volume::fill(root);
        volume::free_and_sync(root);
        println!("L3 native-volume full-code={code} free-write-sync=ok preflight=ok");
    });
}
