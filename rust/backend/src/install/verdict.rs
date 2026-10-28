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
    pub checked: Vec<String>,
    pub items: Vec<(Mark, String)>,
}

/// A checklist line a check that passed records. What is left to do and what failed are the run's steps and its fail,
/// which the checklist prints after these, so it never leaves out what the verdict names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code, reason = "Windows does not record checklist items yet"))]
pub(super) enum Mark {
    Ok,
    Made,
}

impl Mark {
    fn word(self) -> &'static str {
        match self { Self::Ok => "ok", Self::Made => "made" }
    }
}

impl Found {
    #[cfg_attr(windows, allow(dead_code, reason = "Windows does not record checklist items yet"))]
    pub fn item(&mut self, mark: Mark, text: impl Into<String>) {
        self.items.push((mark, text.into()));
    }

    /// A failed check; the first one stands.
    pub fn fail(&mut self, check: &str, detail: impl AsRef<str>) {
        if self.fail.is_none() {
            self.fail = Some(format!("{check}: {}", detail.as_ref()));
        }
    }

    pub fn step(&mut self, text: impl Into<String>) {
        self.steps.push(text.into());
    }

    pub fn checked(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !self.checked.contains(&text) { self.checked.push(text); }
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
    let steps = found.steps.iter().filter(|part| !part.is_empty()).map(String::as_str).collect::<Vec<_>>().join("; then ");
    let (word, head) = if steps.is_empty() { ("PASS", found.pass.as_str()) } else { ("ONE STEP LEFT", steps.as_str()) };
    let details = std::iter::once(head).chain(found.checked.iter().map(String::as_str))
        .chain(std::iter::once(pin.strip_prefix("; ").unwrap_or(pin)))
        .filter(|part| !part.is_empty()).collect::<Vec<_>>().join("; ");
    (format!("{word}: {details}"), 0)
}

/// The verdict for a run that stopped, and its exit code.
pub(super) fn stop_line(stop: &Stop, tail: &str) -> (String, i32) {
    (format!("FAIL: {}: {}{tail}", stop.check, stop.detail), stop.code)
}

/// The run's last output: the checklist, then the verdict line. The checklist is the items in the order they were
/// recorded, then each step left (`todo`), then the fail (`FAIL`), so it holds everything the verdict names; a system
/// that records no items prints the verdict alone, and a stop (a run that ended before its checks) has no checklist.
fn output(outcome: Result<(&Found, &str), &Stop>, tail: &str) -> (String, i32) {
    let (found, pin) = match outcome {
        Ok(pair) => pair,
        Err(stop) => return stop_line(stop, tail),
    };
    let (verdict, code) = line(found, pin);
    if found.items.is_empty() {
        return (verdict, code);
    }
    let one_line = |text: &str| text.replace(['\n', '\r'], " ");
    let mut text = String::from("Checklist:\n");
    for (mark, item) in &found.items {
        text.push_str(&format!("  {}: {}\n", mark.word(), one_line(item)));
    }
    for step in found.steps.iter().filter(|s| !s.is_empty()) {
        text.push_str(&format!("  todo: {}\n", one_line(step)));
    }
    if let Some(fail) = &found.fail {
        text.push_str(&format!("  FAIL: {}\n", one_line(fail)));
    }
    text.push_str(&verdict);
    (text, code)
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
    // A stop is under way, so the step this run ended in failed by it: the watcher's verdict, printed once the step
    // and what it started have ended, is the run's, and the watcher ends the process.
    while super::cmd::STEPS.is_stopping() {
        std::thread::park();
    }
    let (text, code) = match outcome {
        Ok((found, pin)) => output(Ok((&found, &pin)), ""),
        Err(stop) => output(Err(&stop), &tail()),
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
