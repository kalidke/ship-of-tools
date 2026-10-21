//! The one verdict line every run ends with (scripts/CLAUDE.md, Installers: "One verdict"): `PASS: Ship of Tools <v> at
//! <short> on this computer`, `ONE STEP LEFT: <steps>` (exit 0, the install is whole) or `FAIL: <check>: <detail>` (exit
//! 1, or 2 for a refusal). It is printed from here alone: by [`finish`] at the end of a run, or by the signal watcher
//! when a person stops it, whichever comes first.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The stage the run is in, named by a FAIL that has no check of its own (a signal, an early stop).
static STEP: Mutex<&str> = Mutex::new("options");
/// What every FAIL line ends with once the run has written it: the pin, and an update a stage left armed.
static TAIL: Mutex<String> = Mutex::new(String::new());
/// Set by whoever prints the verdict, so exactly one is printed.
static DONE: AtomicBool = AtomicBool::new(false);

pub(super) fn step(name: &'static str) {
    *STEP.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = name;
}

fn current_step() -> &'static str {
    *STEP.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Set the tail every later FAIL line carries.
pub(super) fn set_tail(tail: String) {
    *TAIL.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = tail;
}

fn tail() -> String {
    TAIL.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// A run that ends before its checks: the check (the step it stopped in), why, and the exit code (1, or 2 for a refusal).
#[derive(Debug)]
pub(super) struct Stop {
    pub check: String,
    pub detail: String,
    pub code: i32,
}

impl Stop {
    /// A stop in the current step.
    pub fn new(code: i32, detail: impl Into<String>) -> Self {
        Self { check: current_step().to_string(), detail: detail.into(), code }
    }

    /// A stop that names its own check.
    pub fn at(check: &str, code: i32, detail: impl Into<String>) -> Self {
        Self { check: check.to_string(), detail: detail.into(), code }
    }
}

/// What a run that reached its checks found: the first failed check, the steps a person takes, and what PASS names.
#[derive(Debug, Default)]
pub(super) struct Found {
    pub fail: Option<String>,
    pub steps: Vec<String>,
    pub pass: String,
}

impl Found {
    /// A failed check; the first one stands.
    pub fn fail(&mut self, check: &str, detail: impl AsRef<str>) {
        if self.fail.is_none() {
            self.fail = Some(format!("{check}: {}", detail.as_ref()));
        }
    }

    pub fn step(&mut self, text: impl Into<String>) {
        self.steps.push(text.into());
    }
}

/// What a pinned install's lines end with.
pub(super) fn pin_tail(version: &str) -> String {
    format!("; pinned at {version}: automatic updates stay off until a run that names no version")
}

/// The verdict for a run that reached its checks, and its exit code. `pin` is the pin's tail once the run wrote one.
pub(super) fn line(found: &Found, pin: &str) -> (String, i32) {
    if let Some(fail) = &found.fail {
        return (format!("FAIL: {fail}{pin}"), 1);
    }
    if !found.steps.is_empty() {
        return (format!("ONE STEP LEFT: {}{pin}", found.steps.join("; then ")), 0);
    }
    (format!("PASS: {}{pin}", found.pass), 0)
}

/// The verdict for a run that stopped, and its exit code.
pub(super) fn stop_line(stop: &Stop, tail: &str) -> (String, i32) {
    (format!("FAIL: {}: {}{tail}", stop.check, stop.detail), stop.code)
}

/// Print `text` as the verdict unless one was printed; true when this call printed it.
fn print_once(text: &str) -> bool {
    if DONE.swap(true, Ordering::SeqCst) {
        return false;
    }
    println!("{text}");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    true
}

/// The run's last act: print its verdict and give its exit code.
pub(super) fn finish(outcome: Result<(Found, String), Stop>) -> i32 {
    let (text, code) = match outcome {
        Ok((found, pin)) => line(&found, &pin),
        Err(stop) => stop_line(&stop, &tail()),
    };
    if print_once(&text) {
        code
    } else {
        // A signal printed the verdict while the run ended; its code stands.
        130
    }
}

/// Watch for a person stopping the run (INT, TERM, HUP; Ctrl+C on Windows): the running step is ended with what it
/// started, then the FAIL line names the step, and the run exits 128 plus the signal, which the bootstraps read as a run
/// that printed its verdict.
pub(super) fn watch_signals() {
    std::thread::spawn(|| {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        let code = rt.block_on(async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let (Ok(mut int), Ok(mut term), Ok(mut hup)) =
                    (signal(SignalKind::interrupt()), signal(SignalKind::terminate()), signal(SignalKind::hangup()))
                else {
                    return None;
                };
                tokio::select! {
                    _ = int.recv() => Some(130),
                    _ = term.recv() => Some(143),
                    _ = hup.recv() => Some(129),
                }
            }
            #[cfg(windows)]
            {
                tokio::signal::ctrl_c().await.ok().map(|()| 130)
            }
        });
        let Some(code) = code else { return };
        // The running step and what it started end first, so nothing writes after the verdict and the bootstrap
        // removes a scratch folder no process still uses.
        super::cmd::STEPS.stop(code - 128, std::time::Duration::from_secs(10));
        if print_once(&format!("FAIL: {}: stopped{}", current_step(), tail())) {
            std::process::exit(code);
        }
    });
}

#[cfg(test)]
#[path = "verdict_tests.rs"]
mod tests;
