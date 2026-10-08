//! A full volume under `capsule::run`: the native code reaches the run's caller.
use super::*;
use sot_log::capsule::producer::{ExitStatus, Producer};
use sot_log::capsule::{self, CapsuleConfig, ExitSummary};
use sot_log::host::storage_exhaustion;
use sot_log::lane::wire::Survival;
use sot_log::store::segment::RetentionClass;
use sot_log::Result;
use std::io::{Read, Write};
use std::sync::mpsc;

pub fn codes() -> Vec<i32> {
    #[cfg(unix)]
    {
        vec![libc::ENOSPC, libc::EDQUOT]
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL};
        vec![ERROR_DISK_FULL as i32, ERROR_HANDLE_DISK_FULL as i32]
    }
}

pub fn config(root: &Path, argv: Vec<String>) -> CapsuleConfig {
    CapsuleConfig {
        voyage_root: root.join("voyage"),
        voyage_id: "l3-storage".into(),
        retention: RetentionClass::Discard,
        producer_kind: "l3".into(),
        argv,
        cols: 80,
        rows: 25,
        survival: Survival::Normal,
        parent_lease: None,
        rollout_evidence: sot_log::store::rollout::RolloutEvidence::NoRollbackTarget,
    }
}

/// An output stream that reports end of file only once its sender is dropped,
/// which `close_output_side` does (the `Producer` EOF contract).
pub struct HeldOutput(mpsc::Receiver<()>);
impl Read for HeldOutput {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        let _ = self.0.recv();
        Ok(0)
    }
}

/// A producer that has already exited with `Code(0)` when the loop first asks.
pub struct ExitProducer {
    gate: Option<mpsc::Sender<()>>,
    output: Option<HeldOutput>,
    input: std::io::Sink,
}
impl Producer for ExitProducer {
    type Output = HeldOutput;
    fn pre_spawn_detail() -> serde_json::Value {
        serde_json::json!({})
    }
    fn spawn(_: &[String], _: u16, _: u16) -> Result<Self> {
        let (gate, held) = mpsc::channel();
        Ok(Self {
            gate: Some(gate),
            output: Some(HeldOutput(held)),
            input: std::io::sink(),
        })
    }
    fn take_output(&mut self) -> Self::Output {
        self.output.take().expect("output taken once")
    }
    fn input(&mut self) -> &mut dyn Write {
        &mut self.input
    }
    fn resize(&self, _: u16, _: u16) -> Result<()> {
        Ok(())
    }
    fn wait(&self, _: Duration) -> Result<bool> {
        Ok(true)
    }
    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus> {
        Ok(ExitStatus::Code(0))
    }
    fn terminate_domain(&self) -> Result<()> {
        Ok(())
    }
    fn domain_is_empty(&self) -> Result<bool> {
        Ok(true)
    }
    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        self.gate.take();
        std::thread::spawn(|| {})
    }
}

/// A producer that fills the volume it is started on, then writes output
/// forever: the voyage runs out of space after the run is under way.
pub struct OutputProducer {
    input: std::io::Sink,
}
impl Producer for OutputProducer {
    type Output = std::io::Repeat;
    fn pre_spawn_detail() -> serde_json::Value {
        serde_json::json!({})
    }
    fn spawn(argv: &[String], _: u16, _: u16) -> Result<Self> {
        volume::fill(Path::new(&argv[0]));
        Ok(Self {
            input: std::io::sink(),
        })
    }
    fn take_output(&mut self) -> Self::Output {
        std::io::repeat(b'o')
    }
    fn input(&mut self) -> &mut dyn Write {
        &mut self.input
    }
    fn resize(&self, _: u16, _: u16) -> Result<()> {
        Ok(())
    }
    fn wait(&self, _: Duration) -> Result<bool> {
        Ok(false)
    }
    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus> {
        unreachable!()
    }
    fn terminate_domain(&self) -> Result<()> {
        Ok(())
    }
    fn domain_is_empty(&self) -> Result<bool> {
        Ok(true)
    }
    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(|| {})
    }
}

fn run_exit(root: &Path) -> Result<ExitSummary> {
    let (_tx, rx) = mpsc::channel();
    capsule::run::<ExitProducer>(
        config(root, vec!["exit".into()]),
        rx,
        &mut transports::NoopTransport,
    )
}

/// The run caller's error must be the native one: its code is a storage code.
fn assert_native_code(result: Result<ExitSummary>, label: &str) -> i32 {
    let error = result.expect_err(label);
    let code = storage_exhaustion(&error);
    assert!(
        code.is_some_and(|code| codes().contains(&code)),
        "L3 {label}: storage exhaustion must reach the run caller with its native code, got {error:?}"
    );
    code.unwrap()
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs the bounded ext4 volume of rust.yml's L3 step"
)]
fn a_full_volume_under_an_existing_voyage_returns_the_native_code() {
    volume::on_volume(|root| {
        run_exit(root).expect("a run on a healthy volume");
        let filled = volume::fill(root);
        let code = assert_native_code(run_exit(root), "existing-voyage");
        volume::free_and_sync(root);
        println!("L3 existing-voyage fill-code={filled} run-code={code} caller=storage_exhaustion cleanup=ok");
    });
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs the bounded ext4 volume of rust.yml's L3 step"
)]
fn a_full_volume_under_a_fresh_root_returns_the_native_code() {
    volume::on_volume(|root| {
        let filled = volume::fill(root);
        let code = assert_native_code(run_exit(root), "fresh-root");
        volume::free_and_sync(root);
        println!(
            "L3 fresh-root fill-code={filled} run-code={code} caller=storage_exhaustion cleanup=ok"
        );
    });
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs the bounded ext4 volume of rust.yml's L3 step"
)]
fn a_full_volume_after_output_returns_the_native_code() {
    volume::on_volume(|root| {
        let (_tx, rx) = mpsc::channel();
        let cfg = config(root, vec![root.to_string_lossy().into_owned()]);
        let result = capsule::run::<OutputProducer>(cfg, rx, &mut transports::NoopTransport);
        let code = assert_native_code(result, "after-output");
        volume::free_and_sync(root);
        println!("L3 after-output run-code={code} caller=storage_exhaustion cleanup=ok");
    });
}
