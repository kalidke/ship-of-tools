//! The REPL model and routing owner; State applies transport and presentation effects.

use crate::net::dial::HostKey;
use crate::net::transport::OutgoingReq;
use crate::ui::WsKey;
use log::ReplEntry;
use std::collections::HashMap;

pub(in crate::ui) mod lines;
pub(in crate::ui) mod log;
mod replies;

/// Row-local runtime state. The counter and interrupt memory deliberately do not travel with it.
#[derive(Clone, Default)]
struct ReplRow {
    log: Vec<ReplEntry>,
    input: String,
    pkg_mode: bool,
    scroll: u16,
    history_pos: Option<usize>,
    history_saved: Option<String>,
}

#[derive(Default)]
pub(in crate::ui) struct ReplModel {
    live: ReplRow,
    parked: HashMap<WsKey, ReplRow>,
    count: u64,
    routes: HashMap<(HostKey, u64), WsKey>,
    runfile_status: HashMap<(HostKey, u64), (String, Option<String>, bool)>,
    interrupted: Vec<u64>,
    lifecycle: HashMap<WsKey, String>,
}

/// State supplies its existing normalization and wire selection before borrowing the owner.
pub(in crate::ui) struct ReplTarget {
    pub(in crate::ui) host: HostKey,
    pub(in crate::ui) workspace_id: Option<String>,
    pub(in crate::ui) view_key: WsKey,
}

pub(in crate::ui) enum DraftEdit<'a> {
    Typed(&'a str),
    Paste(&'a str),
    Newline,
    Space,
    Backspace,
    Replace(String),
}

enum SubmissionKind {
    Eval,
    File { basename: String, fresh: bool },
}

pub(in crate::ui) struct Submission {
    eval_id: u64,
    kind: SubmissionKind,
}

impl Submission {
    pub(in crate::ui) fn eval_id(&self) -> u64 {
        self.eval_id
    }
}

pub(in crate::ui) struct ReplSend {
    pub(in crate::ui) host: HostKey,
    pub(in crate::ui) request: OutgoingReq,
    pub(in crate::ui) submission: Submission,
}

pub(in crate::ui) struct ReplInterrupt {
    pub(in crate::ui) host: HostKey,
    workspace_id: Option<String>,
    ids: Vec<u64>,
    force: bool,
}

impl ReplInterrupt {
    pub(in crate::ui) fn request(&self) -> OutgoingReq {
        OutgoingReq::ReplInterrupt {
            workspace_id: self.workspace_id.clone(),
            eval_ids: self.ids.clone(),
            force: self.force,
        }
    }
}

pub(in crate::ui) struct BrowserAction {
    pub(in crate::ui) host: HostKey,
    pub(in crate::ui) url: String,
    pub(in crate::ui) open: bool,
    pub(in crate::ui) fe: Option<String>,
}

#[derive(Default)]
pub(in crate::ui) struct ReplEffects {
    pub(in crate::ui) status: Option<String>,
    pub(in crate::ui) redraw: bool,
    pub(in crate::ui) listing: Option<HostKey>,
    pub(in crate::ui) browser: Option<BrowserAction>,
    pub(in crate::ui) clear_images: bool,
    pub(in crate::ui) reset_build_anchor: bool,
}

impl ReplModel {
    pub(in crate::ui) fn entries(&self) -> &[ReplEntry] {
        &self.live.log
    }
    pub(in crate::ui) fn input(&self) -> &str {
        &self.live.input
    }
    pub(in crate::ui) fn pkg_mode(&self) -> bool {
        self.live.pkg_mode
    }
    pub(in crate::ui) fn scroll(&self) -> u16 {
        self.live.scroll
    }
    #[cfg(test)]
    pub(in crate::ui) fn parked_entries(&self, key: &WsKey) -> Option<&[ReplEntry]> {
        self.parked.get(key).map(|row| row.log.as_slice())
    }

    pub(in crate::ui) fn capture_row(&mut self, key: WsKey) {
        self.parked.insert(key, self.live.clone());
    }

    pub(in crate::ui) fn restore_row(&mut self, key: &WsKey) -> (bool, ReplEffects) {
        let row = self.parked.get(key).cloned();
        let found = row.is_some();
        self.live = row.unwrap_or_default();
        (
            found,
            ReplEffects {
                reset_build_anchor: true,
                ..Default::default()
            },
        )
    }

    pub(in crate::ui) fn migrate_default_key(&mut self, from: &WsKey, to: WsKey) {
        if let Some(row) = self.parked.remove(from) {
            self.parked.entry(to.clone()).or_insert(row);
        }
        for ((host, _), key) in &mut self.routes {
            if host == &from.0 && key == from {
                *key = to.clone();
            }
        }
        // Lifecycle remains in raw-slug space.
    }

    pub(in crate::ui) fn forget_destroyed_row(&mut self, key: &WsKey) {
        self.parked.remove(key);
    }

    pub(in crate::ui) fn apply_listing_catchup(&mut self, entries: HashMap<WsKey, String>) {
        // Presentation exports only nonempty values. Keep unmentioned frame-driven state;
        // a mentioned value can overwrite it, without a freshness arbitration policy.
        for (key, state) in entries {
            self.lifecycle.insert(key, state);
        }
    }

    pub(in crate::ui) fn starting(&self, active: &WsKey, default_slug: Option<&str>) -> bool {
        let slug = if active.1 == "<default>" {
            default_slug.unwrap_or(&active.1)
        } else {
            &active.1
        };
        matches!(
            self.lifecycle
                .get(&(active.0.clone(), slug.to_string()))
                .map(String::as_str),
            Some("starting")
        )
    }
}

#[cfg(test)]
mod model_tests;
