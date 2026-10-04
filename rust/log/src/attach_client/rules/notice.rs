//! Rulings (e) and (f): the attach notice and the `fe_down` marker.

// ---------------------------------------------------------------------
// (e) The attach notice is bound to the leg it describes
// ---------------------------------------------------------------------

/// The one truthful attach-notice message, given the confirmed leg's own
/// creation time formatted by the caller (this module carries no clock
/// formatting opinion — the runtime supplies an already-rendered
/// timestamp string).
pub fn attach_notice_text(leg_started_at: &str) -> String {
    format!("attached to leg started {leg_started_at}")
}

// ---------------------------------------------------------------------
// (f) `fe_down` claims only what it can observe
// ---------------------------------------------------------------------

/// Reads the `ts` field of the LAST well-formed JSON line in `contents`
/// (an already-loaded `fe-inbox.jsonl`) — "last inbox evidence." `None`
/// when the file is empty or its last line is not a JSON object with a
/// string `ts` field: an unusable trailing line makes no evidence claim,
/// rather than reaching further back and claiming an OLDER line is the
/// most recent evidence (which would be false).
pub fn last_evidence_ts(contents: &str) -> Option<String> {
    let last = contents.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(last.trim()).ok()?;
    v.get("ts").and_then(|t| t.as_str()).map(|s| s.to_string())
}

/// The exact JSON shape ADR 0041 pins for the marker line:
/// `{"from":"sot-fe","to":"<handle>","text":"possible relay gap: last
/// inbox evidence <t0>, frontend reattached <t1>","ts":"<t1>",
/// "kind":"fe_down","window":{"last_evidence":"<t0>"}}`. `to` is the
/// durable inbox's own addressee handle; `last_evidence`/`reattach_ts`
/// are both ISO-8601 strings, carried verbatim (this function does no
/// time parsing or formatting of its own).
pub fn build_fe_down_marker(to: &str, last_evidence: &str, reattach_ts: &str) -> serde_json::Value {
    serde_json::json!({
        "from": "sot-fe",
        "to": to,
        "text": format!(
            "possible relay gap: last inbox evidence {last_evidence}, frontend reattached {reattach_ts}"
        ),
        "ts": reattach_ts,
        "kind": "fe_down",
        "window": { "last_evidence": last_evidence },
    })
}

/// `t0`, read at FE PROCESS START before this run appends anything —
/// "No baseline, no marker." Also tracks whether this process has
/// already made its first attach ("skipped on a first attach": the very
/// first attach of a process's life never emits a marker, since a
/// process that has not yet attached even once cannot itself have
/// missed traffic during ITS OWN prior downtime — the gap the marker
/// reports is always relative to a PRIOR attach).
#[derive(Debug, Clone)]
pub struct FeDownBaseline {
    last_evidence: Option<String>,
    first_attach_done: bool,
}

impl FeDownBaseline {
    /// `last_evidence`: the result of [`last_evidence_ts`] over
    /// `fe-inbox.jsonl`'s content as read at FE PROCESS START (before
    /// this run's own first append of ANY kind — the frontend calls
    /// this from `State::new`, never from drawer-open time; see
    /// `fe_client_io`'s own module doc for the Codex review finding
    /// this fixes).
    pub fn capture(last_evidence: Option<String>) -> Self {
        Self { last_evidence, first_attach_done: false }
    }

    /// Called on every successful attach. Returns the marker to append,
    /// or `None` when no marker should be written this time (no
    /// baseline, or this is the process's first attach).
    pub fn marker_for_attach(&mut self, to: &str, reattach_ts: &str) -> Option<serde_json::Value> {
        let first = !self.first_attach_done;
        self.first_attach_done = true;
        if first {
            return None;
        }
        let t0 = self.last_evidence.as_deref()?;
        Some(build_fe_down_marker(to, t0, reattach_ts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- (e) attach notice ------------------------------------------------

    #[test]
    fn attach_notice_text_is_the_pinned_wording() {
        assert_eq!(attach_notice_text("2026-09-01T12:00:00Z"), "attached to leg started 2026-09-01T12:00:00Z");
    }

    // ---- (f) fe_down --------------------------------------------------

    #[test]
    fn last_evidence_ts_reads_the_last_lines_ts() {
        let contents = "{\"ts\":\"t-old\"}\n{\"ts\":\"t-new\"}\n";
        assert_eq!(last_evidence_ts(contents).as_deref(), Some("t-new"));
    }

    #[test]
    fn last_evidence_ts_none_on_empty_or_malformed_trailing_line() {
        assert_eq!(last_evidence_ts(""), None);
        assert_eq!(last_evidence_ts("not json\n"), None);
        assert_eq!(last_evidence_ts("{\"ts\":\"t-old\"}\nnot json\n"), None);
    }

    #[test]
    fn marker_shape_matches_the_pinned_json() {
        let v = build_fe_down_marker("backend-dev", "t0", "t1");
        assert_eq!(v["from"], "sot-fe");
        assert_eq!(v["to"], "backend-dev");
        assert_eq!(v["kind"], "fe_down");
        assert_eq!(v["ts"], "t1");
        assert_eq!(v["window"]["last_evidence"], "t0");
        assert_eq!(v["text"], "possible relay gap: last inbox evidence t0, frontend reattached t1");
    }

    #[test]
    fn no_baseline_no_marker() {
        let mut b = FeDownBaseline::capture(None);
        // Even on a SECOND attach (so "first attach" is not why), no
        // baseline still means no marker.
        b.marker_for_attach("h", "t1");
        assert!(b.marker_for_attach("h", "t2").is_none());
    }

    #[test]
    fn first_attach_is_skipped_even_with_a_baseline() {
        let mut b = FeDownBaseline::capture(Some("t0".to_string()));
        assert!(b.marker_for_attach("h", "t1").is_none());
        let second = b.marker_for_attach("h", "t2").expect("second attach should mark");
        assert_eq!(second["window"]["last_evidence"], "t0");
        assert_eq!(second["ts"], "t2");
    }
}
