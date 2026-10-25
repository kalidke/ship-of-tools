//! Listed row presentation, activity order and transition flashes. Identity stays in RowDirectory.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::row_directory::RowDirectory;
use crate::net::dial::HostKey;
use crate::ui::WsKey;

#[derive(Default)]
pub(in crate::ui) struct RowPresentation {
    workspace_labels: HashMap<WsKey, String>,
    workspace_project_roots: HashMap<WsKey, String>,
    workspace_states: HashMap<WsKey, (String, String)>,
    prev_workspace_states: HashMap<WsKey, String>,
    flash_starts: HashMap<WsKey, Instant>,
    workspace_slugs: Vec<WsKey>,
    default_workspace_slug: Option<String>,
}

pub(in crate::ui) struct PresentationEffects {
    pub(in crate::ui) repl_catchup: HashMap<WsKey, String>,
    pub(in crate::ui) default_migration: Option<WsKey>,
}

const FLASH_SECS: f32 = 0.6;

impl RowPresentation {
    pub(in crate::ui) fn refresh(
        &mut self,
        directory: &RowDirectory,
        ordered_hosts: &[HostKey],
        active_host: &str,
        active_slug: Option<&str>,
        badged: HashSet<WsKey>,
        now: Instant,
    ) -> PresentationEffects {
        let mut labels = HashMap::new();
        let mut roots = HashMap::new();
        let mut states = HashMap::new();
        let mut slugs = Vec::new();
        let mut default_slug = None;
        let mut repl_catchup = HashMap::new();
        for host in ordered_hosts {
            for row in directory.rows(host).unwrap_or_default() {
                let key = (host.clone(), row.slug.clone());
                if !row.is_inert_anchor() {
                    let label = if row.label.is_empty() {
                        &row.slug
                    } else {
                        &row.label
                    };
                    labels.insert(key.clone(), label.clone());
                    slugs.push(key.clone());
                }
                roots.insert(
                    key.clone(),
                    row.canonical_root
                        .as_ref()
                        .unwrap_or(&row.project_root)
                        .clone(),
                );
                states.insert(
                    key.clone(),
                    (row.agent_state.clone(), row.agent_status_at.clone()),
                );
                if !row.repl_state.is_empty() {
                    repl_catchup.insert(key.clone(), row.repl_state.clone());
                }
                if row.is_default && host == active_host {
                    default_slug = Some(row.slug.clone());
                }
            }
        }
        for (key, (state, _)) in &states {
            if self
                .prev_workspace_states
                .get(key)
                .is_some_and(|previous| previous != state)
            {
                self.flash_starts.insert(key.clone(), now);
            }
            self.prev_workspace_states
                .insert(key.clone(), state.clone());
        }
        let selected = selected_key(active_host, active_slug, default_slug.as_deref());
        self.workspace_slugs = activity_order(
            &slugs,
            &states,
            &badged,
            &self.workspace_slugs,
            selected.as_ref(),
        );
        self.workspace_labels = labels;
        self.workspace_project_roots = roots;
        self.workspace_states = states;
        self.default_workspace_slug = default_slug;
        let default_migration = self
            .default_slug()
            .map(|slug| (active_host.to_string(), slug.to_string()));
        PresentationEffects {
            repl_catchup,
            default_migration,
        }
    }

    pub(in crate::ui) fn resort(&mut self, host: &str, slug: Option<&str>, badged: HashSet<WsKey>) {
        let selected = self.selected_key(host, slug);
        self.workspace_slugs = activity_order(
            &self.workspace_slugs,
            &self.workspace_states,
            &badged,
            &self.workspace_slugs,
            selected.as_ref(),
        );
    }

    pub(in crate::ui) fn select_host_default(&mut self, directory: &RowDirectory, host: &str) {
        self.default_workspace_slug = directory
            .rows(host)
            .and_then(|rows| rows.iter().find(|row| row.is_default))
            .map(|row| row.slug.clone());
    }

    pub(in crate::ui) fn keys(&self) -> &[WsKey] {
        &self.workspace_slugs
    }

    pub(in crate::ui) fn default_slug(&self) -> Option<&str> {
        self.default_workspace_slug.as_deref()
    }

    pub(in crate::ui) fn selected_key(&self, host: &str, slug: Option<&str>) -> Option<WsKey> {
        selected_key(host, slug, self.default_slug())
    }

    pub(in crate::ui) fn next_row(
        &self,
        direction: i32,
        host: &str,
        slug: Option<&str>,
    ) -> Option<WsKey> {
        if self.workspace_slugs.len() < 2 {
            return None;
        }
        let selected = self.selected_key(host, slug);
        let idx = selected
            .as_ref()
            .and_then(|key| self.workspace_slugs.iter().position(|k| k == key))
            .unwrap_or(0) as i32;
        let next = (idx + direction).rem_euclid(self.workspace_slugs.len() as i32) as usize;
        Some(self.workspace_slugs[next].clone())
    }

    pub(in crate::ui) fn label(&self, host: &str, slug: &str) -> Option<&str> {
        self.workspace_labels
            .get(&(host.to_string(), slug.to_string()))
            .map(String::as_str)
    }

    pub(in crate::ui) fn state(&self, host: &str, slug: &str) -> Option<(&str, &str)> {
        self.workspace_states
            .get(&(host.to_string(), slug.to_string()))
            .map(|(state, at)| (state.as_str(), at.as_str()))
    }

    pub(in crate::ui) fn project_root(&self, host: &str, slug: &str) -> Option<&str> {
        self.workspace_project_roots
            .get(&(host.to_string(), slug.to_string()))
            .map(String::as_str)
    }

    pub(in crate::ui) fn active_label(
        &self,
        host: &str,
        slug: Option<&str>,
        hello_basename: Option<&str>,
    ) -> Option<String> {
        match slug {
            Some(slug) => Some(self.label(host, slug).unwrap_or(slug).to_string()),
            None => hello_basename.map(str::to_string),
        }
    }

    pub(in crate::ui) fn active_project_root<'a>(
        &'a self,
        host: &str,
        slug: Option<&str>,
        hello_root: Option<&'a str>,
    ) -> Option<&'a str> {
        match slug {
            Some(slug) => match self.project_root(host, slug) {
                Some(root) => Some(root),
                None if self.default_slug() == Some(slug) => hello_root,
                None => None,
            },
            None => hello_root,
        }
    }

    pub(in crate::ui) fn forget_destroyed_metadata(&mut self, key: &WsKey) {
        self.workspace_labels.remove(key);
        self.workspace_project_roots.remove(key);
    }

    pub(in crate::ui) fn flash_factor_for(&self, host: &str, slug: &str, now: Instant) -> f32 {
        self.flash_starts
            .get(&(host.to_string(), slug.to_string()))
            .map(|start| {
                (1.0 - now.duration_since(*start).as_secs_f32() / FLASH_SECS).clamp(0.0, 1.0)
            })
            .unwrap_or(0.0)
    }

    pub(in crate::ui) fn has_flashes(&self) -> bool {
        !self.flash_starts.is_empty()
    }

    pub(in crate::ui) fn prune_expired_flashes(&mut self, now: Instant) -> bool {
        let window = Duration::from_secs_f32(FLASH_SECS);
        self.flash_starts
            .retain(|_, start| now.duration_since(*start) < window);
        self.has_flashes()
    }

    pub(in crate::ui) fn seed_capture(
        &mut self,
        sessions: &[String],
        states: &[Option<String>],
        flash_slugs: &[String],
        status_at: &str,
        now: Instant,
    ) -> Option<WsKey> {
        let host = "demo".to_string();
        if !sessions.is_empty() {
            self.workspace_slugs = sessions.iter().map(|s| (host.clone(), s.clone())).collect();
            for (i, slug) in sessions.iter().enumerate() {
                let key = (host.clone(), slug.clone());
                self.workspace_labels.insert(key.clone(), slug.clone());
                if let Some(Some(state)) = states.get(i) {
                    self.workspace_states
                        .insert(key.clone(), (state.clone(), status_at.to_string()));
                    self.prev_workspace_states.insert(key, state.clone());
                }
            }
        }
        for slug in flash_slugs {
            let key = (host.clone(), slug.clone());
            self.flash_starts.insert(key.clone(), now);
            let prior = match self.workspace_states.get(&key) {
                Some((current, _)) if current == "idle" => "working",
                _ => "idle",
            };
            self.prev_workspace_states.insert(key, prior.to_string());
        }
        sessions
            .get(sessions.len() / 2)
            .map(|slug| (host, slug.clone()))
    }
}

fn selected_key(host: &str, slug: Option<&str>, default: Option<&str>) -> Option<WsKey> {
    slug.or(default).map(|s| (host.to_string(), s.to_string()))
}

fn activity_rank(state: &str, badged: bool) -> u8 {
    match (state, badged) {
        ("blocked", _) => 0,
        (_, true) => 1,
        ("done", _) => 2,
        ("working", _) => 3,
        ("waiting", _) => 4,
        _ => 5,
    }
}

fn activity_order(
    slugs: &[WsKey],
    states: &HashMap<WsKey, (String, String)>,
    badged: &std::collections::HashSet<WsKey>,
    prev: &[WsKey],
    pinned: Option<&WsKey>,
) -> Vec<WsKey> {
    let prev_pos = |k: &WsKey| prev.iter().position(|p| p == k).unwrap_or(usize::MAX);
    let sort_key = |k: &WsKey| {
        let (state, at) = states
            .get(k)
            .map(|(s, a)| (s.as_str(), a.as_str()))
            .unwrap_or(("", ""));
        let stamp = chrono::DateTime::parse_from_rfc3339(at)
            .ok()
            .map(|t| t.timestamp_millis())
            .unwrap_or(i64::MIN);
        (
            activity_rank(state, badged.contains(k)),
            std::cmp::Reverse(stamp),
            prev_pos(k),
        )
    };
    let mut out: Vec<WsKey> = Vec::with_capacity(slugs.len());
    let mut start = 0;
    while start < slugs.len() {
        let host = &slugs[start].0;
        let end = start + slugs[start..].iter().take_while(|k| k.0 == *host).count();
        let block = &slugs[start..end];
        let pin = pinned.filter(|p| block.contains(p)).and_then(|p| {
            prev.iter()
                .filter(|k| k.0 == *host)
                .position(|k| k == p)
                .map(|slot| (p, slot.min(block.len() - 1)))
        });
        let mut ranked: Vec<&WsKey> = block
            .iter()
            .filter(|k| pin.map_or(true, |(p, _)| *k != p))
            .collect();
        ranked.sort_by_cached_key(|k| sort_key(k));
        if let Some((p, slot)) = pin {
            ranked.insert(slot, p);
        }
        out.extend(ranked.into_iter().cloned());
        start = end;
    }
    out
}

#[cfg(test)]
#[path = "row_presentation_tests.rs"]
mod tests;
