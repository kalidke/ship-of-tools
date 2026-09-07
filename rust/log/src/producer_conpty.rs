//! `impl Producer for ConptyProducer` — wraps the owned-ConPTY primitives
//! `conpty.rs` hands out (`ConptySpawn`'s destructured fields) behind the
//! [`crate::producer::Producer`] trait, so `capsule::run` can drive a
//! ConPTY producer through the same nine verbs any other platform's
//! producer answers. Every method here is the one-line delegation the
//! writer loop did directly before LU2a (ADR 0043 "Decisions for LU2");
//! nothing here changes what OS calls are made or in what order — the
//! loop's own behavior on Windows is unchanged.

#![cfg(windows)]

use crate::conpty::{observe_spawning_process_jobbed, AnonymousJob, ConptySpawn, PrimaryProcess, Pseudoconsole};
use crate::producer::{ExitStatus, Producer};
use crate::Result;
use serde_json::json;
use std::fs::File;
use std::io::Write;
use std::time::Duration;

/// Wraps one `ConptySpawn`'s fields. `pty` is `Option` so
/// [`close_output_side`](Producer::close_output_side) can take it out on
/// the closer thread while `job`/`process` (and `writer`, for the
/// host-handshake reply the loop keeps answering through the drain) stay
/// live and reachable on `self` throughout. `reader` is `Option` for the
/// identical reason [`take_output`](Producer::take_output) requires: a
/// value taken exactly once.
pub struct ConptyProducer {
    job: AnonymousJob,
    process: PrimaryProcess,
    pty: Option<Pseudoconsole>,
    reader: Option<File>,
    writer: File,
}

impl Producer for ConptyProducer {
    type Output = File;

    fn pre_spawn_detail() -> serde_json::Value {
        // The current `producer_spawn.detail` shape, unchanged (ADR 0041):
        // observed independent of spawn's own outcome (see
        // `observe_spawning_process_jobbed`'s own doc), so a failed spawn
        // still records it.
        json!({"spawning_process_was_jobbed": observe_spawning_process_jobbed()})
    }

    fn spawn(argv: &[String], cols: u16, rows: u16) -> Result<Self> {
        let ConptySpawn {
            job,
            process,
            pty,
            reader,
            writer,
            pid: _,
            detail: _,
        } = ConptySpawn::spawn(argv, cols, rows)?;
        Ok(Self {
            job,
            process,
            pty: Some(pty),
            reader: Some(reader),
            writer,
        })
    }

    fn take_output(&mut self) -> Self::Output {
        self.reader.take().expect("ConptyProducer::take_output called twice")
    }

    fn input(&mut self) -> &mut dyn Write {
        &mut self.writer
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.pty
            .as_ref()
            .expect("ConptyProducer::resize called after close_output_side")
            .resize(cols, rows)
    }

    fn wait(&self, timeout: Duration) -> Result<bool> {
        self.process.wait(timeout)
    }

    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus> {
        Ok(ExitStatus::Code(self.process.exit_code_after_confirmed_exit()?))
    }

    fn terminate_domain(&self) -> Result<()> {
        self.job.terminate()
    }

    fn domain_is_empty(&self) -> Result<bool> {
        Ok(self.job.active_processes()? == 0)
    }

    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        let pty = self.pty.take().expect("ConptyProducer::close_output_side called twice");
        std::thread::spawn(move || pty.close_pty())
    }
}
