use super::*;
use std::collections::{HashMap, HashSet};

/// The window's drift cache for one workspace, driven through the same gates the real code calls.
#[derive(Default)]
struct Cache {
    hashes: HashMap<String, String>,
    fired: HashSet<String>,
    retry: HashMap<String, (std::time::Instant, u32)>,
    target: Option<String>,
}

impl Cache {
    /// The cursor reaches `path`: true when a `file.parse` goes out.
    fn view(&mut self, path: &str) -> bool {
        take_parse_fire(path, &self.hashes, &mut self.fired, &self.retry)
    }
    fn reply(&mut self, path: &str, hash: &str) -> bool {
        install_parsed_hash(path, hash.to_string(), &mut self.hashes, &self.fired, &mut self.retry)
    }
    /// A `preview.changed` for `node_id`, applied as `forget_changed_file` applies it.
    fn changed(&mut self, node_id: &str) {
        match change_effect(node_id, self.target.as_deref()) {
            ChangeEffect::ReadAnnotationAgain => self.target = None,
            ChangeEffect::ForgetFile(p) => forget_drift(p, &mut self.hashes, &mut self.fired, &mut self.retry),
            ChangeEffect::Nothing => {}
        }
    }
    fn stale(&self, path: &str, synced: &str) -> bool {
        drift_is_stale(&self.hashes, path, synced)
    }
}

/// The whole sequence: view, parse, edit, parse again (STALE), reconcile (cleared), with no restart.
#[test]
fn edit_turns_stale_and_reconcile_clears_it() {
    let mut c = Cache::default();
    assert!(c.view("a.jl"), "the first view parses");
    assert!(!c.view("a.jl"), "a view while the parse is in flight does not fire again");
    assert!(c.reply("a.jl", "h1"));
    assert!(!c.view("a.jl"), "a hashed file is not parsed again");
    assert!(!c.stale("a.jl", "h1"));
    c.changed("files:a.jl");
    assert!(c.view("a.jl"), "the view after an edit parses again");
    assert!(c.reply("a.jl", "h2"));
    assert!(c.stale("a.jl", "h1"), "the new hash differs from synced_against");
    // The reconcile rewrites the annotation: the cursored one is read again, and its new synced_against agrees.
    c.target = Some("files/a.jl".to_string());
    c.changed("files:.concept/files/a.jl.md");
    assert_eq!(c.target, None);
    assert!(!c.stale("a.jl", "h2"));
}

/// A reply read before the edit arrives after the forget: dropped, so the old hash never lands.
#[test]
fn reply_in_flight_across_an_edit_is_dropped() {
    let mut c = Cache::default();
    assert!(c.view("a.jl"));
    c.changed("files:a.jl");
    assert!(!c.reply("a.jl", "old"));
    assert!(c.hashes.is_empty());
    assert!(c.view("a.jl"));
    assert!(c.reply("a.jl", "new"));
    assert_eq!(c.hashes.get("a.jl").map(String::as_str), Some("new"));
    // A reply for a path never latched (a Modules-mode parse) installs nothing.
    assert!(!c.reply("/abs/m.jl", "x"));
}

/// An event for one file, or for another file's annotation, leaves this file's hash alone.
#[test]
fn change_to_another_file_keeps_this_files_hash() {
    let mut c = Cache::default();
    for p in ["a.jl", "b.jl"] {
        c.view(p);
        c.reply(p, "h");
    }
    c.target = Some("files/a.jl".to_string());
    c.changed("files:b.jl");
    c.changed("files:.concept/files/b.jl.md");
    assert!(c.hashes.contains_key("a.jl") && c.fired.contains("a.jl"));
    assert_eq!(c.target.as_deref(), Some("files/a.jl"));
    assert!(!c.hashes.contains_key("b.jl"));
}

/// A switch or reconnect drops every hash and latch (their edits may have gone unseen), so each file parses
/// again at its next view.
#[test]
fn restore_and_reconnect_clear_the_cache() {
    let mut c = Cache::default();
    c.view("a.jl");
    c.reply("a.jl", "h1");
    c.view("b.jl");
    c.retry.insert("b.jl".to_string(), (std::time::Instant::now(), 2));
    clear_drift(&mut c.hashes, &mut c.fired, &mut c.retry);
    assert!(c.retry.is_empty());
    assert!(!c.reply("b.jl", "late"), "an in-flight reply from before the clear is dropped");
    assert!(c.view("a.jl") && c.view("b.jl"));
}

/// A failed parse is re-armed only after its backoff and not past the cap.
#[test]
fn failed_parse_waits_out_its_backoff() {
    let mut c = Cache::default();
    assert!(c.view("a.jl"));
    c.retry.insert("a.jl".to_string(), (std::time::Instant::now(), 1));
    assert!(!c.view("a.jl"));
    let old = std::time::Instant::now() - std::time::Duration::from_secs(10);
    c.retry.insert("a.jl".to_string(), (old, 1));
    assert!(c.view("a.jl"));
    c.retry.insert("a.jl".to_string(), (old, FILE_PARSE_MAX_RETRIES));
    assert!(!c.view("a.jl"), "at the cap the latch stays");
}

#[test]
fn annotation_node_ids_name_their_target() {
    assert_eq!(annotation_target_of("files:.concept/files/src/x.jl.md").as_deref(), Some("files/src/x.jl"));
    assert_eq!(annotation_target_of("files:.concept/modules/Foo.md").as_deref(), Some("modules/Foo"));
    assert_eq!(annotation_target_of("files:src/x.jl"), None);
    assert_eq!(annotation_target_of("files:.concept/.md"), None);
}

/// A closing fence with no newline after it is the end of the file: the header keeps exactly what was
/// there, so header plus body is the original and no final newline appears.
#[test]
fn concept_frontmatter_closing_fence_at_eof_preserves_bytes() {
    for s in [
        "---\ntarget: x\n---",
        "---\r\ntarget: x\r\n---",
        "---\ntarget: x\n---\nbody",
        "# no header",
    ] {
        let (h, b) = split_frontmatter(s);
        assert_eq!(h.unwrap_or_default() + &b, s);
    }
    assert_eq!(
        split_frontmatter("---\nt: x\n---").0.as_deref(),
        Some("---\nt: x\n---")
    );
    assert_eq!(
        split_frontmatter("---\nt: x\nbody"),
        (None, "---\nt: x\nbody".to_string())
    );
}
