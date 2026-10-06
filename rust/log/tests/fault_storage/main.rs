//! Precommit P1/P2 premises. No production change is committed by this suite.
#![cfg(any(target_os = "linux", target_os = "macos", windows))]

mod boundaries;
mod surface;
mod volume;

use std::path::{Path, PathBuf};
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

fn bounded(mut command: Command, seconds: u64) -> (i32, String) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("start the owned premise command");
    let (status, out, err) =
        sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(seconds));
    (status.code().unwrap_or(-1), scrub(&format!("{out}{err}")))
}

fn scrub(text: &str) -> String {
    let mut text = text.to_string();
    for (value, replacement) in [
        (std::env::var_os("CARGO_TARGET_DIR"), "<target>"),
        (
            Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../..")
                    .into_os_string(),
            ),
            "<repo>",
        ),
        (std::env::var_os("HOME"), "<home>"),
        (std::env::var_os("USERPROFILE"), "<home>"),
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
fn premises_p1_parent_prospective_reversal() {
    surface::boundary_gate();
}

#[test]
fn premises_p2_native_volume() {
    volume::on_volume("premises_p2_native_volume", |root| {
        sot_log::host::preflight_volume(root)
            .expect("P2 native volume must pass capsule preflight");
        let code = volume::fill(root);
        volume::free_and_sync(root);
        println!("L3 P2 native-volume full-code={code} free-write-sync=ok preflight=ok");
    });
}
