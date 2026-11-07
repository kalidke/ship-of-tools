//! Authoritative host listings and their identities. Presentation caches stay with the session view.

use std::collections::{HashMap, HashSet};

use crate::net::dial::HostKey;
use crate::net::transport::{WorkspaceDestroyedInfo, WorkspaceInfo};
use crate::ui::WsKey;

use super::workspace_key::{is_default_workspace_name, ws_key_of};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::ui) struct ResultRowIdentity {
    pub(in crate::ui) host: HostKey,
    pub(in crate::ui) workspace_id: String,
}

/// An owned target: resolving never leaves a directory borrow across a State transition.
#[derive(Clone, Debug)]
pub(in crate::ui) struct ResolvedWorkspace {
    identity: ResultRowIdentity,
    row_key: WsKey,
    view_key: WsKey,
    is_default: bool,
    session_name: String,
    visible: bool,
}

impl ResolvedWorkspace {
    pub(in crate::ui) fn identity(&self) -> &ResultRowIdentity {
        &self.identity
    }

    pub(in crate::ui) fn row_key(&self) -> &WsKey {
        &self.row_key
    }

    pub(in crate::ui) fn view_key(&self) -> &WsKey {
        &self.view_key
    }

    pub(in crate::ui) fn slug(&self) -> Option<String> {
        (!self.is_default).then(|| self.row_key.1.clone())
    }

    pub(in crate::ui) fn visible(&self) -> bool {
        self.visible
    }

    pub(in crate::ui) fn attachment(&self) -> Result<Option<String>, String> {
        if !self.visible {
            Ok(None)
        } else if self.session_name.is_empty() {
            Err("listed workspace has no attachment target".to_string())
        } else {
            Ok(Some(self.session_name.clone()))
        }
    }
}

/// Only an authoritative listing can replace rows. A disconnected host keeps its last listing.
#[derive(Default)]
pub(in crate::ui) struct RowDirectory {
    workspace_lists: HashMap<HostKey, Vec<WorkspaceInfo>>,
    workspace_id_slugs: HashMap<(HostKey, String), WsKey>,
    /// Rows the window destroyed or is destroying that their host's listing still has.
    destroyed: HashSet<WsKey>,
}

/// How a row of a host went: the window's own destroy was answered for it, or the host's listing no longer has it.
pub(in crate::ui) enum Gone<'a> {
    Replied(&'a str),
    Listed(&'a RowDirectoryUpdate),
}

/// Where the window's view goes when its selected row may be gone.
#[derive(Debug, PartialEq)]
pub(in crate::ui) enum Leave {
    Stay,
    To(WsKey),
    /// No other row to show: the host's default row, and an empty pane when it has no attachment.
    NoSession,
}

/// Prepared without changing the directory, so the coordinator can reconcile results before commitment.
pub(in crate::ui) struct RowDirectoryUpdate {
    host: HostKey,
    rows: Vec<WorkspaceInfo>,
    retired: Vec<ResultRowIdentity>,
}

impl RowDirectoryUpdate {
    pub(in crate::ui) fn retired(&self) -> &[ResultRowIdentity] {
        &self.retired
    }

    /// Whether the new listing still has the row.
    fn lists(&self, slug: &str) -> bool {
        self.rows.iter().any(|row| row.slug == slug)
    }
}

impl RowDirectory {
    /// Replacing one host never retires another host's same slug or legacy id. Compare canonical ids,
    /// so a same-slug successor retires its predecessor and a retained default row keeps its identity.
    pub(in crate::ui) fn prepare_replace(
        &self,
        host: HostKey,
        rows: Vec<WorkspaceInfo>,
    ) -> RowDirectoryUpdate {
        let mut retired = Vec::new();
        for previous in self.rows(&host).unwrap_or_default() {
            if !rows
                .iter()
                .any(|row| row.workspace_id == previous.workspace_id)
            {
                let identity = ResultRowIdentity {
                    host: host.clone(),
                    workspace_id: previous.workspace_id.clone(),
                };
                if !retired.contains(&identity) {
                    retired.push(identity);
                }
            }
        }
        RowDirectoryUpdate {
            host,
            rows,
            retired,
        }
    }

    /// Commit immediately after the coordinator's reconciliation, before rebuilding projections.
    /// Rebuild translation from those same authoritative listings, including invisible anchors.
    /// Every installed host participates, even a synthetic host outside State's fixed connections;
    /// display order scopes presentation projections, not the directory's identity translation.
    pub(in crate::ui) fn commit_replace(&mut self, update: RowDirectoryUpdate) {
        // A note lives until a listing lacks its row (the destroy happened) or the daemon refuses the destroy.
        self.destroyed.retain(|(host, slug)| *host != update.host || update.lists(slug));
        self.workspace_lists.insert(update.host, update.rows);
        self.workspace_id_slugs.clear();
        for (host, rows) in &self.workspace_lists {
            for row in rows {
                self.workspace_id_slugs.insert(
                    (host.clone(), row.workspace_id.clone()),
                    (host.clone(), row.slug.clone()),
                );
            }
        }
    }

    /// A destroy the window made (requested, or answered) before the host's next listing: the row, named by slug or
    /// workspace id, is no longer a place to go. The next listing of the host replaces the record.
    pub(in crate::ui) fn note_destroyed(&mut self, host: &str, spelling: &str) {
        let slug = self
            .rows(host)
            .and_then(|rows| rows.iter().find(|row| row.slug == spelling || row.workspace_id == spelling))
            .map_or(spelling, |row| row.slug.as_str())
            .to_string();
        self.destroyed.insert((host.to_string(), slug));
    }

    /// The daemon's answer to a destroy: a refusal names no row, so every destroy noted for the host is alive until a
    /// listing says otherwise; a kept row (the default, whose run ended) is alive; any other row is gone.
    pub(in crate::ui) fn destroy_answered(&mut self, host: &str, answer: &Result<WorkspaceDestroyedInfo, String>) {
        match answer {
            Err(_) => self.clear_destroyed(host),
            Ok(info) if info.kept.is_some() => {
                self.destroyed.remove(&(host.to_string(), info.slug.clone()));
            }
            Ok(info) => self.note_destroyed(host, &info.slug),
        }
    }

    /// The slugs the host listed before that the update's listing lacks: rows destroyed elsewhere.
    pub(in crate::ui) fn dropped(&self, host: &str, update: &RowDirectoryUpdate) -> Vec<String> {
        let rows = self.rows(host).unwrap_or_default();
        rows.iter().filter(|row| !update.lists(&row.slug)).map(|row| row.slug.clone()).collect()
    }

    /// A destroy whose outcome is unknown (the daemon refused it, or the connection ended before it answered): the
    /// host's rows noted as destroyed are taken as alive.
    pub(in crate::ui) fn clear_destroyed(&mut self, host: &str) {
        self.destroyed.retain(|(h, _)| h != host);
    }

    /// What the window does with its selected row `selected` (host, slug) once a row of `event_host` is gone, by the
    /// window's own destroy reply or by the host's next listing (`Gone`). Called before `commit_replace`, which
    /// consumes the update. A row of another host never moves the view, and a row never listed (a selection resumed
    /// before the first list) is not gone. The neighbor is the next session in the whole Sessions list, which is the
    /// hosts' listings in `hosts` order, each in the daemon's order (default first, then by slug): below the selected
    /// one, else above it, among visible rows that are still listed and not destroyed or being destroyed. With no
    /// such row anywhere, `NoSession`.
    pub(in crate::ui) fn leave(
        &self,
        hosts: &[HostKey],
        selected: (&str, Option<&str>),
        event_host: &str,
        gone: Gone<'_>,
    ) -> Leave {
        let (host, Some(slug)) = selected else { return Leave::Stay };
        if host != event_host {
            return Leave::Stay;
        }
        let listed = |slug: &str| self.rows(event_host).is_some_and(|rows| rows.iter().any(|row| row.slug == slug));
        let is_gone = match gone {
            Gone::Replied(replied) => replied == slug,
            Gone::Listed(update) => listed(slug) && !update.lists(slug),
        };
        if !is_gone {
            return Leave::Stay;
        }
        let alive = |host: &str, row: &WorkspaceInfo| {
            !self.destroyed.contains(&(host.to_string(), row.slug.clone()))
                && (host != event_host || matches!(gone, Gone::Replied(_)) || matches!(gone, Gone::Listed(u) if u.lists(&row.slug)))
        };
        let whole: Vec<(&str, &WorkspaceInfo)> = hosts
            .iter()
            .flat_map(|h| self.rows(h).unwrap_or_default().iter().map(move |row| (h.as_str(), row)))
            .collect();
        let Some(at) = whole.iter().position(|(h, row)| *h == event_host && row.slug == slug) else {
            return Leave::Stay;
        };
        let below = whole[at + 1..].iter();
        let above = whole[..at].iter().rev();
        below
            .chain(above)
            .find(|(h, row)| !row.is_inert_anchor() && alive(h, row))
            .map_or(Leave::NoSession, |(h, row)| Leave::To((h.to_string(), row.slug.clone())))
    }

    /// None distinguishes a host that has not answered from one whose authoritative list is empty.
    pub(in crate::ui) fn rows(&self, host: &str) -> Option<&[WorkspaceInfo]> {
        self.workspace_lists.get(host).map(Vec::as_slice)
    }

    /// The native pane fixture reports every known listing when setup times out.
    #[cfg(all(test, feature = "test-pane-timing"))]
    pub(in crate::ui) fn listings(&self) -> impl Iterator<Item = (&HostKey, &[WorkspaceInfo])> {
        self.workspace_lists
            .iter()
            .map(|(host, rows)| (host, rows.as_slice()))
    }

    /// Strict command admission: a spelling must name exactly one row with a nonempty, unique id
    /// on the specified dial host. Attachment names are copied from the listing, never derived.
    pub(in crate::ui) fn resolve(
        &self,
        host: &HostKey,
        spelling: &str,
    ) -> Result<ResolvedWorkspace, String> {
        let list = self
            .rows(host)
            .ok_or_else(|| "workspace list unavailable".to_string())?;
        let mut matches = list.iter().filter(|row| {
            (is_default_workspace_name(spelling) && row.is_default)
                || row.slug == spelling
                || row.workspace_id == spelling
        });
        let row = matches
            .next()
            .ok_or_else(|| "unknown workspace".to_string())?;
        if matches.next().is_some() || row.workspace_id.is_empty() {
            return Err("ambiguous or empty workspace identity".to_string());
        }
        if list
            .iter()
            .filter(|other| other.workspace_id == row.workspace_id)
            .count()
            != 1
        {
            return Err("ambiguous workspace identity".to_string());
        }
        Ok(ResolvedWorkspace {
            identity: ResultRowIdentity {
                host: host.clone(),
                workspace_id: row.workspace_id.clone(),
            },
            row_key: (host.clone(), row.slug.clone()),
            view_key: (
                host.clone(),
                ws_key_of(Some(&row.slug), row.is_default.then_some(row.slug.as_str())),
            ),
            is_default: row.is_default,
            session_name: row.session_name.clone(),
            visible: !row.is_inert_anchor(),
        })
    }

    /// Legacy lifecycle lookup is deliberately tolerant: unknown hints may already be slugs.
    /// An absent hint uses the caller's existing default-slug fallback, even for another event host.
    pub(in crate::ui) fn lifecycle_key(
        &self,
        host: &str,
        wire_hint: Option<&str>,
        default_slug: Option<&str>,
    ) -> WsKey {
        match wire_hint {
            None => (
                host.to_string(),
                default_slug.unwrap_or("<default>").to_string(),
            ),
            Some(hint) => self
                .workspace_id_slugs
                .get(&(host.to_string(), hint.to_string()))
                .cloned()
                .unwrap_or_else(|| (host.to_string(), hint.to_string())),
        }
    }
}

#[cfg(test)]
#[path = "row_directory_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "workspace_key_listed_tests.rs"]
mod listed_tests;
