//! `impl Producer for ConptyProducer` — wraps the owned-ConPTY primitives
//! `capsule/producer/conpty/` hands out (`ConptySpawn`'s destructured fields) behind the
//! [`crate::capsule::producer::Producer`] trait, so `capsule::run` can drive a
//! ConPTY producer through the same nine verbs any other platform's
//! producer answers. Every method here is the one-line delegation the
//! writer loop did directly before LU2a (ADR 0043 "Decisions for LU2");
//! nothing here changes what OS calls are made or in what order — the
//! loop's own behavior on Windows is unchanged.

#![cfg(windows)]

use crate::capsule::producer::conpty::{ConptySpawn, PrimaryProcess, Pseudoconsole};
use sot_platform::host::winjob::AnonymousJob;
use crate::capsule::producer::{ExitStatus, Producer};
use crate::Result;
use std::fs::File;
use std::time::Duration;

/// Wraps one `ConptySpawn`'s fields. `pty` is `Option` so
/// [`close_output_side`](Producer::close_output_side) can take it out on
/// the closer thread while `job`/`process` (and `writer`, for the
/// host-handshake reply the loop keeps answering through the drain) stay
/// live and reachable on `self` throughout. `reader` is `Option` for the
/// identical reason [`take_output`](Producer::take_output) requires: a
/// value taken exactly once.
pub struct ConptyProducer {
    // FIELD ORDER IS LOAD-BEARING. Struct fields drop in declaration order,
    // and this order reproduces what the loop's destructured locals did
    // before LU2a (locals drop in reverse declaration order: writer, reader,
    // pty, process, job): on an early-error unwind the input handle closes
    // first, then the pseudoconsole, then the process handle, and the job —
    // whose KILL_ON_JOB_CLOSE terminates the tree — goes LAST. Declaring
    // `job` first would kill the producer before its input closed, changing
    // the EOF-versus-forced-termination ordering on every error path.
    writer: File,
    reader: Option<File>,
    pty: Option<Pseudoconsole>,
    process: PrimaryProcess,
    job: AnonymousJob,
}

impl Producer for ConptyProducer {
    type Output = File;

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
            writer,
            reader: Some(reader),
            pty: Some(pty),
            process,
            job,
        })
    }

    fn take_output(&mut self) -> Self::Output {
        self.reader.take().expect("ConptyProducer::take_output called twice")
    }

    fn input_writer(&self) -> std::io::Result<std::fs::File> {
        self.writer.try_clone()
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
        Ok(self.job.terminate()?)
    }

    fn domain_is_empty(&self) -> Result<bool> {
        Ok(self.job.active_processes()? == 0)
    }

    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        let pty = self.pty.take().expect("ConptyProducer::close_output_side called twice");
        std::thread::spawn(move || pty.close_pty())
    }
}
