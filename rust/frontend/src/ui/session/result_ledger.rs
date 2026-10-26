//! Runtime results owed to canonical rows, their attempts and presentation certificates.

use std::collections::{HashMap, HashSet};

use crate::net::dial::HostKey;
use crate::net::transport::ResultAttemptId;
use crate::ui::WsKey;

use super::{ResolvedWorkspace, ResultRowIdentity, RowDirectory};

#[derive(Clone, Debug, PartialEq)]
struct ResultAttempt {
    serial: u64,
    node_id: String,
    preview_gen: u64,
    cursor_landed: bool,
    preview_installed: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingNav {
    path: String,
    result_serial: u64,
    attempt: Option<ResultAttempt>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct PendingResults {
    entries: HashMap<ResultRowIdentity, PendingNav>,
    strip: HashSet<WsKey>,
}

/// The only mutable home of owed results, monotonic serials and result reveal ownership.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::ui) struct ResultLedger {
    pending_nav: PendingResults,
    result_serial: u64,
    attempt_serial: u64,
    result_reveal: Option<ResultAttemptId>,
}

pub(in crate::ui) struct AttemptStart {
    pub(in crate::ui) id: ResultAttemptId,
    pub(in crate::ui) node_id: String,
    pub(in crate::ui) preview_gen: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum ResultRowOutcome {
    Kept,
    Removed,
}

/// State applies navigation cleanup before directory commitment, and refreshes presentation itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct ResultEffects {
    pub(in crate::ui) refresh_badges: bool,
    pub(in crate::ui) clear_reveal: bool,
}

impl ResultLedger {
    pub(in crate::ui) fn record(
        &mut self,
        directory: &RowDirectory,
        host: &HostKey,
        spelling: &str,
        path: String,
    ) -> Result<(), String> {
        let target = directory.resolve(host, spelling)?;
        let serial = self
            .result_serial
            .checked_add(1)
            .ok_or("result serial space exhausted")?;
        self.result_serial = serial;
        self.pending_nav.entries.insert(
            target.identity().clone(),
            PendingNav {
                path,
                result_serial: serial,
                attempt: None,
            },
        );
        Ok(())
    }

    pub(in crate::ui) fn contains_identity(&self, identity: &ResultRowIdentity) -> bool {
        self.pending_nav.entries.contains_key(identity)
    }

    #[cfg(test)]
    pub(in crate::ui) fn path(&self, identity: &ResultRowIdentity) -> Option<&str> {
        self.pending_nav
            .entries
            .get(identity)
            .map(|entry| entry.path.as_str())
    }

    pub(in crate::ui) fn has_badge(&self, key: &WsKey) -> bool {
        self.pending_nav.strip.contains(key)
    }

    pub(in crate::ui) fn badged_keys(&self, directory: &RowDirectory) -> HashSet<WsKey> {
        self.pending_nav
            .entries
            .keys()
            .filter_map(|id| {
                let row = directory
                    .rows(&id.host)?
                    .iter()
                    .find(|row| row.workspace_id == id.workspace_id)?;
                Some((id.host.clone(), row.slug.clone()))
            })
            .collect()
    }

    pub(in crate::ui) fn refresh_badges(&mut self, directory: &RowDirectory) {
        self.pending_nav.strip = self.badged_keys(directory);
    }

    /// Check exhaustion before lookup; consume neither clock when there is no owed entry.
    pub(in crate::ui) fn begin_attempt(
        &mut self,
        target: &ResolvedWorkspace,
        issue_preview: impl FnOnce() -> u64,
    ) -> Result<Option<AttemptStart>, &'static str> {
        let serial = self
            .attempt_serial
            .checked_add(1)
            .ok_or("result attempt serial space exhausted")?;
        let Some(entry) = self.pending_nav.entries.get_mut(target.identity()) else {
            return Ok(None);
        };
        self.attempt_serial = serial;
        let preview_gen = issue_preview();
        let node_id = format!("files:{}", entry.path);
        entry.attempt = Some(ResultAttempt {
            serial,
            node_id: node_id.clone(),
            preview_gen,
            cursor_landed: false,
            preview_installed: false,
        });
        Ok(Some(AttemptStart {
            id: ResultAttemptId {
                workspace_id: target.identity().workspace_id.clone(),
                result_serial: entry.result_serial,
                attempt_serial: serial,
            },
            node_id,
            preview_gen,
        }))
    }

    /// Called after the preview request was sent; the issuing token stays hostless.
    pub(in crate::ui) fn arm_reveal(&mut self, attempt: ResultAttemptId) {
        self.result_reveal = Some(attempt);
    }

    pub(in crate::ui) fn reveal_attempt(&self) -> Option<&ResultAttemptId> {
        self.result_reveal.as_ref()
    }

    /// Switching, ordinary opens and failed result trees release only the reveal marker.
    pub(in crate::ui) fn release_reveal(&mut self) {
        self.result_reveal = None;
    }

    pub(in crate::ui) fn abandon_attempt(
        &mut self,
        directory: &RowDirectory,
        active_host: &HostKey,
        attempt: &ResultAttemptId,
    ) {
        if let Some(entry) = self.attempt_entry_mut(directory, active_host, attempt) {
            entry.attempt = None;
        }
        self.release_reveal();
    }

    fn attempt_entry_mut(
        &mut self,
        directory: &RowDirectory,
        host: &HostKey,
        attempt: &ResultAttemptId,
    ) -> Option<&mut PendingNav> {
        let target = directory.resolve(host, &attempt.workspace_id).ok()?;
        let entry = self.pending_nav.entries.get_mut(target.identity())?;
        let current = entry.attempt.as_ref()?;
        (target.identity().workspace_id == attempt.workspace_id
            && entry.result_serial == attempt.result_serial
            && current.serial == attempt.attempt_serial)
            .then_some(entry)
    }

    pub(in crate::ui) fn admits_attempt(
        &mut self,
        directory: &RowDirectory,
        event_host: &HostKey,
        active_host: &HostKey,
        attempt: &ResultAttemptId,
    ) -> bool {
        event_host == active_host
            && self.result_reveal.as_ref() == Some(attempt)
            && self
                .attempt_entry_mut(directory, event_host, attempt)
                .is_some()
    }

    pub(in crate::ui) fn cursor_landed(
        &mut self,
        directory: &RowDirectory,
        active_host: &HostKey,
        node_id: &str,
    ) {
        let Some(id) = self.result_reveal.clone() else {
            return;
        };
        let landed = self
            .attempt_entry_mut(directory, active_host, &id)
            .and_then(|entry| entry.attempt.as_mut());
        if let Some(attempt) = landed.filter(|attempt| attempt.node_id == node_id) {
            attempt.cursor_landed = true;
            self.release_reveal();
        }
    }

    pub(in crate::ui) fn preview_installed(
        &mut self,
        target: Option<&ResolvedWorkspace>,
        generation: u64,
        node_id: Option<&str>,
    ) {
        let attempt = target
            .and_then(|target| self.pending_nav.entries.get_mut(target.identity()))
            .and_then(|entry| entry.attempt.as_mut());
        if let Some(a) = attempt {
            if a.preview_gen == generation && node_id == Some(a.node_id.as_str()) {
                a.preview_installed = true;
            }
        }
    }

    /// Called only after presentation; certificates alone never consume an owed result.
    pub(in crate::ui) fn acknowledge_presented(
        &mut self,
        target: Option<&ResolvedWorkspace>,
        cursor_id: Option<&str>,
        shown_id: Option<&str>,
    ) -> ResultEffects {
        let Some(target) = target else {
            return ResultEffects::default();
        };
        let attempt = self
            .pending_nav
            .entries
            .get(target.identity())
            .and_then(|entry| entry.attempt.as_ref());
        if attempt.is_some_and(|a| {
            a.cursor_landed
                && a.preview_installed
                && shown_id == Some(a.node_id.as_str())
                && cursor_id == Some(a.node_id.as_str())
        }) {
            self.pending_nav.entries.remove(target.identity());
            return ResultEffects {
                refresh_badges: true,
                clear_reveal: false,
            };
        }
        ResultEffects::default()
    }

    pub(in crate::ui) fn reconcile_retired(
        &mut self,
        retired: &[ResultRowIdentity],
        active_host: &HostKey,
    ) -> ResultEffects {
        let mut effects = ResultEffects::default();
        for identity in retired {
            if self.contains_identity(identity) {
                let removed = self.invalidate(identity, active_host);
                effects.refresh_badges |= removed.refresh_badges;
                effects.clear_reveal |= removed.clear_reveal;
            }
        }
        effects
    }

    pub(in crate::ui) fn row_outcome(
        &mut self,
        identity: &ResultRowIdentity,
        outcome: ResultRowOutcome,
        active_host: &HostKey,
    ) -> ResultEffects {
        match outcome {
            ResultRowOutcome::Kept => ResultEffects::default(),
            ResultRowOutcome::Removed => self.invalidate(identity, active_host),
        }
    }

    fn invalidate(&mut self, identity: &ResultRowIdentity, active_host: &HostKey) -> ResultEffects {
        self.pending_nav.entries.remove(identity);
        let clear_reveal = &identity.host == active_host
            && self
                .result_reveal
                .as_ref()
                .is_some_and(|a| a.workspace_id == identity.workspace_id);
        if clear_reveal {
            self.release_reveal();
        }
        ResultEffects {
            refresh_badges: true,
            clear_reveal,
        }
    }
}

#[cfg(test)]
#[path = "result_ledger_tests.rs"]
mod tests;
