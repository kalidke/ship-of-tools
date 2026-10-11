//! Storage exhaustion on a real bounded volume: what a full volume does to the capsule's callers.
#![cfg(any(target_os = "linux", target_os = "macos", windows))]

mod boundaries;
#[cfg(any(target_os = "macos", windows))]
mod command_tests;
mod exits;
#[cfg(any(windows, target_os = "linux"))]
mod scenario;
#[allow(dead_code)]
#[path = "../support/transports.rs"]
mod transports;
mod volume;

use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", windows))]
use std::process::{Command, Stdio};
use std::time::Duration;

/// The fixtures' scratch folder, under the system temp folder.
fn scratch() -> PathBuf {
    let scratch = std::env::temp_dir().join("sot-l3-scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    scratch
}

#[cfg(any(target_os = "macos", windows))]
fn bounded(mut command: Command, seconds: u64) -> (i32, String) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap_or_else(|error| {
        panic!(
            "L3 HARNESS FAILURE: start the owned premise command: {}",
            scrub(&error.to_string())
        )
    });
    finish_bounded(child, Duration::from_secs(seconds))
}

#[cfg(any(target_os = "macos", windows))]
fn finish_bounded(child: std::process::Child, bound: Duration) -> (i32, String) {
    let (status, out, err) = sot_log::test_isolated::drain(child)
        .try_wait_within(bound)
        .unwrap_or_else(|error| panic!("L3 HARNESS FAILURE: native command: {}", scrub(&error)));
    (status.code().unwrap_or(-1), scrub(&format!("{out}{err}")))
}

#[cfg(any(target_os = "macos", windows))]
fn scrub(text: &str) -> String {
    let mut text = text
        .split_inclusive('\n')
        .map(|line| {
            let (body, ending) = if let Some(body) = line.strip_suffix("\r\n") {
                (body, "\r\n")
            } else if let Some(body) = line.strip_suffix('\n') {
                (body, "\n")
            } else {
                (line, "")
            };
            if let Some((prefix, _)) = body.split_once("On computer:") {
                format!("{prefix}On computer: <host>{ending}")
            } else {
                line.to_owned()
            }
        })
        .collect::<String>();
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
