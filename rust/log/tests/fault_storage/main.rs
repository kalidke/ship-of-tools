//! Storage exhaustion on a real bounded volume: what a full volume does to the capsule's callers.
#![cfg(any(target_os = "linux", target_os = "macos", windows))]

mod boundaries;
#[cfg(any(target_os = "macos", windows))]
mod command_tests;
#[cfg(any(target_os = "macos", test))]
mod detach;
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
struct NativeObservation {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

#[cfg(any(target_os = "macos", windows))]
impl NativeObservation {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    fn diagnostic(&self, label: &str) -> String {
        scrub(&format!(
            "{label} status={} code={:?} success={}\n{label} stdout:\n{}\n{label} stderr:\n{}",
            self.status,
            self.status.code(),
            self.status.success(),
            self.stdout,
            self.stderr
        ))
    }

    fn exit_record(&self) -> String {
        self.status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".into())
    }
}

#[cfg(any(target_os = "macos", windows))]
fn bounded(mut command: Command, seconds: u64, owned: &[(&str, &str)]) -> NativeObservation {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap_or_else(|error| {
        panic!(
            "L3 HARNESS FAILURE: start the owned premise command: {}; command status=unavailable; command stdout: unavailable; command stderr: unavailable",
            scrub_owned(&error.to_string(), owned)
        )
    });
    finish_bounded(child, Duration::from_secs(seconds), owned)
}

#[cfg(any(target_os = "macos", windows))]
fn finish_bounded(
    child: std::process::Child,
    bound: Duration,
    owned: &[(&str, &str)],
) -> NativeObservation {
    let (status, out, err) = sot_log::test_isolated::drain(child)
        .try_wait_within(bound)
        .unwrap_or_else(|error| {
            panic!(
                "L3 HARNESS FAILURE: native command: {}",
                scrub_owned(&error, owned)
            )
        });
    NativeObservation {
        status,
        stdout: scrub_owned(&out, owned),
        stderr: scrub_owned(&err, owned),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn scrub(text: &str) -> String {
    scrub_owned(text, &[])
}

#[cfg(any(target_os = "macos", windows))]
fn scrub_owned(text: &str, owned: &[(&str, &str)]) -> String {
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
    let mut owned = owned.to_vec();
    owned.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
    for (value, replacement) in owned {
        if !value.is_empty() {
            text = text.replace(value, replacement);
        }
    }
    let mut roots: Vec<_> = [
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
    ]
    .into_iter()
    .filter_map(|(value, replacement)| {
        value.map(|value| (value.to_string_lossy().into_owned(), replacement))
    })
    .collect();
    roots.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
    for (value, replacement) in roots {
        if !value.is_empty() {
            text = text.replace(&value, replacement);
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
