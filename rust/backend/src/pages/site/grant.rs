//! Reply authority belongs to one entry, independently of the site's file roots.

use super::*;
use sot_protocol::ReplyScope;

pub(super) struct Grant {
    pub site: Arc<Site>,
    entry: String,
    scope: ReplyScope,
    navigation_secret: Option<String>,
}

impl Grant {
    pub fn ordinary(site: Arc<Site>) -> Self {
        Self { site, entry: String::new(), scope: ReplyScope::None, navigation_secret: None }
    }

    pub fn new(site: Site, entry: &str, scope: ReplyScope) -> Option<Self> {
        if scope == ReplyScope::None { return Some(Self::ordinary(Arc::new(site))); }
        Some(Self {
            site: Arc::new(site), entry: entry_path(entry), scope,
            navigation_secret: Some(random_token()?),
        })
    }

    pub fn scope_at(&self, path: &str) -> ReplyScope {
        if entry_path(path) == self.entry { self.scope.clone() } else { ReplyScope::None }
    }

    pub fn is_direct(&self) -> bool { self.scope != ReplyScope::None }

    pub fn navigation_secret(&self) -> Option<&str> { self.navigation_secret.as_deref() }

    pub fn is_navigation_secret(&self, secret: &str) -> bool {
        self.navigation_secret().is_some_and(|key| ct_eq(key.as_bytes(), secret.as_bytes()))
    }
}

fn entry_path(path: &str) -> String {
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.ends_with('/') { format!("{path}index.html") } else { path.into() }
}

/// Preserve the page's other query parameters when replacing its pool capability.
pub(super) fn navigation_target(target: &str, secret: &str) -> String {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut parts: Vec<_> = query.split('&').filter(|part| {
        !part.is_empty() && !percent_decode(part.split('=').next().unwrap_or("")).eq_ignore_ascii_case("secret")
    }).collect();
    let key = format!("secret={secret}");
    parts.push(&key);
    format!("{path}?{}", parts.join("&"))
}
