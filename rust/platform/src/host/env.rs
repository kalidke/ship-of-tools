//! The environment as a value: one snapshot a program takes at its entry and passes down, so code that reads a variable
//! takes it from the value it is given and a test builds its own, never writing the process's.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

/// A set of variables. [`Env::process`] is the one read of the process environment; every other form is built.
#[derive(Clone, Debug, Default)]
pub struct Env(BTreeMap<OsString, OsString>);

/// The key as stored: Windows compares variable names without regard to case, so the store keeps them in upper case.
fn key(k: &OsStr) -> OsString {
    if cfg!(windows) { k.to_string_lossy().to_uppercase().into() } else { k.to_os_string() }
}

impl Env {
    /// The process environment as it is now.
    pub fn process() -> Env {
        Env(std::env::vars_os().map(|(k, v)| (key(&k), v)).collect())
    }

    /// No variable set.
    pub fn empty() -> Env {
        Env::default()
    }

    /// `self` with `k` set to `v`.
    pub fn with(mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> Env {
        self.0.insert(key(k.as_ref()), v.as_ref().to_os_string());
        self
    }

    /// Sets `k` to `v` in place.
    pub fn set(&mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) {
        self.0.insert(key(k.as_ref()), v.as_ref().to_os_string());
    }

    /// `self` without `k`.
    pub fn without(mut self, k: impl AsRef<OsStr>) -> Env {
        self.0.remove(&key(k.as_ref()));
        self
    }

    /// The value of `k` when it is set, empty or not.
    pub fn get(&self, k: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.0.get(&key(k.as_ref())).map(OsString::as_os_str)
    }

    /// The value of `k` when it is set and not empty.
    pub fn get_nonempty(&self, k: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.get(k).filter(|v| !v.is_empty())
    }

    /// The value of `k` as text, empty when it is unset or not text.
    pub fn text(&self, k: impl AsRef<OsStr>) -> String {
        self.get(k).and_then(OsStr::to_str).unwrap_or_default().to_string()
    }

    /// Every variable, for a child's `Command::envs`.
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.0.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }
}

#[cfg(test)]
#[path = "env_tests.rs"]
mod tests;
