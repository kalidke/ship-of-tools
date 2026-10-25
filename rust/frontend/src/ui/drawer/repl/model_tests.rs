//! Behavioral owner proofs. No window, transport worker, process or mutable-storage fixture is needed.
use super::{DraftEdit, ReplModel, ReplSend, ReplTarget};
use crate::net::transport::{OutgoingReq, ReplRunFileInfo};
use crate::ui::{ws_info, ReplEntry, ReplFrame, RowDirectory, RowPresentation, WsKey};

fn key(host: &str, row: &str) -> WsKey {
    (host.into(), row.into())
}
fn target(row: &WsKey) -> ReplTarget {
    ReplTarget {
        host: row.0.clone(),
        workspace_id: (row.1 != "<default>").then(|| row.1.clone()),
        view_key: row.clone(),
    }
}
fn begin(model: &mut ReplModel, row: &WsKey, code: &str) -> ReplSend {
    model.edit_draft(DraftEdit::Replace(code.into()));
    model.begin_eval(target(row)).unwrap()
}
fn submit(model: &mut ReplModel, row: &WsKey, code: &str) -> u64 {
    let plan = begin(model, row, code);
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    id
}
fn switch(model: &mut ReplModel, from: &WsKey, to: &WsKey) {
    model.capture_row(from.clone());
    model.restore_row(to);
}

fn output(model: &mut ReplModel, host: &str, id: u64, active: &WsKey, text: &str) {
    model.apply_frame(
        host.into(),
        id,
        active,
        active.clone(),
        ReplFrame::Stdout { text: text.into() },
    );
}
fn done(model: &mut ReplModel, host: &str, id: u64, active: &WsKey) {
    model.apply_frame(
        host.into(),
        id,
        active,
        active.clone(),
        ReplFrame::Done {
            eval_id: id,
            elapsed_ms: 17,
        },
    );
}
fn complete(model: &mut ReplModel, row: &WsKey, id: u64) {
    model.apply_eval_completion(row.0.clone(), id, row, 1, Vec::new());
}
fn texts(log: &[ReplEntry]) -> Vec<String> {
    log.iter()
        .flat_map(|e| &e.frames)
        .map(|f| format!("{f:?}"))
        .collect()
}
fn announce(model: &mut ReplModel, row: &WsKey, active: &WsKey, id: u64) {
    model.start_run(
        row.0.clone(),
        id,
        active,
        row.clone(),
        "session".into(),
        "run file.jl".into(),
    );
}
fn info(id: u64, fresh: bool, elapsed_ms: u64, frames: Vec<ReplFrame>) -> ReplRunFileInfo {
    ReplRunFileInfo {
        eval_id: id,
        path: "folder/file.jl".into(),
        fresh,
        elapsed_ms,
        project_dir: Some("project".into()),
        project_source: Default::default(),
        frames,
    }
}
fn interrupt(model: &mut ReplModel, row: &WsKey, success: bool) -> (Vec<u64>, bool) {
    let plan = model.prepare_interrupt(target(row)).unwrap();
    let OutgoingReq::ReplInterrupt {
        eval_ids,
        force,
        workspace_id,
    } = plan.request()
    else {
        panic!("interrupt request")
    };
    assert_eq!(workspace_id, target(row).workspace_id);
    model.complete_interrupt(Some(plan), success);
    (eval_ids, force)
}

#[test]
fn a_row_switch_never_restarts_the_windows_eval_count() {
    let mut m = ReplModel::default();
    let (a, b) = (key("host", "a"), key("host", "b"));
    assert_eq!(submit(&mut m, &a, "a"), 1);
    m.capture_row(a.clone());
    let (found, effects) = m.restore_row(&b);
    assert!(!found);
    assert!(effects.reset_build_anchor);
    assert_eq!(submit(&mut m, &b, "b"), 2);
    m.capture_row(b);
    let (found, effects) = m.restore_row(&a);
    assert!(found);
    assert!(effects.reset_build_anchor);
    m.clear_scrollback();
    assert_eq!(submit(&mut m, &a, "a again"), 3);
    let plan = m.begin_file_run(target(&a), "file.jl".into(), true);
    assert_eq!(plan.submission.eval_id(), 4);
}

#[test]
fn submission_registers_before_send_and_reports_send_failure() {
    let mut m = ReplModel::default();
    let (a, b) = (key("host", "a"), key("host", "b"));
    m.edit_draft(DraftEdit::Replace(" \n\t".into()));
    assert!(m.begin_eval(target(&a)).is_none());
    assert_eq!(m.input(), "");
    assert!(m.entries().is_empty());
    let p = begin(&mut m, &a, "value");
    let id = p.submission.eval_id();
    assert_eq!(id, 1);
    assert!(
        matches!(&p.request, OutgoingReq::ReplEval { eval_id: 1, code, mode: None, workspace_id: Some(ws) } if code == "value" && ws == "a")
    );
    assert_eq!(p.host, "host");
    assert_eq!(m.in_flight_ids(), [id]);
    assert_eq!(m.entries()[0].code, "value");
    assert_eq!(m.input(), "");
    // Before any send completion, a reply must already find the parked owner.
    switch(&mut m, &a, &b);
    output(&mut m, "host", id, &b, "before send completed");
    assert_eq!(m.parked_entries(&a).unwrap()[0].frames.len(), 1);
    m.restore_row(&a);
    assert!(m
        .complete_submission(p.submission, Err("closed".into()))
        .status
        .is_none());
    let e = &m.entries()[0];
    assert!(!e.in_flight);
    assert!(
        matches!(&e.frames[1], ReplFrame::Error { message, .. } if message == "transport channel closed: closed")
    );
    // Send failure retains the route, even after another switch.
    switch(&mut m, &a, &b);
    output(&mut m, "host", id, &b, "late");
    assert_eq!(m.parked_entries(&a).unwrap()[0].frames.len(), 3);
    for _ in 0..260 {
        submit(&mut m, &b, "bounded");
    }
    assert_eq!(m.entries().len(), 256);
    assert_eq!(m.entries()[0].eval_id, 6);
}

#[test]
fn registered_replies_return_to_the_sending_row() {
    let mut m = ReplModel::default();
    let (a, b, c) = (key("one", "row"), key("one", "other"), key("two", "row"));
    let id = submit(&mut m, &a, "local");
    switch(&mut m, &a, &b);
    m.apply_eval_completion("one".into(), id, &b, 0, vec![]);
    assert!(m.parked_entries(&a).unwrap()[0].in_flight);
    output(&mut m, "one", id, &b, "owned a");
    done(&mut m, "one", id, &b);
    assert!(m.entries().is_empty());
    m.restore_row(&a);
    assert_eq!(m.entries()[0].elapsed_ms, 17);
    assert_eq!(texts(m.entries()).len(), 1);
    // Equal external ids belong to separate dial hosts.
    announce(&mut m, &a, &a, 900);
    switch(&mut m, &a, &c);
    announce(&mut m, &c, &c, 900);
    m.apply_eval_completion("one".into(), 900, &c, 0, vec![]);
    output(&mut m, "one", 900, &c, "one");
    output(&mut m, "two", 900, &c, "two");
    assert_eq!(
        texts(m.entries()),
        [format!("{:?}", ReplFrame::Stdout { text: "two".into() })]
    );
    assert!(texts(m.parked_entries(&a).unwrap())
        .last()
        .unwrap()
        .contains("one"));
    // A recorded owner with no parked record must not fall back to this same-id live entry.
    m.forget_destroyed_row(&a);
    output(&mut m, "one", 900, &c, "must drop");
    assert_eq!(m.entries()[0].frames.len(), 1);
    done(&mut m, "one", 900, &c);
    assert!(m.entries()[0].in_flight);
    done(&mut m, "two", 900, &c);
    assert!(!m.entries()[0].in_flight);
}

#[test]
fn unknown_owner_keeps_the_legacy_live_log_fallback() {
    let mut m = ReplModel::default();
    let a = key("host", "row");
    announce(&mut m, &a, &a, 800);
    done(&mut m, "host", 800, &a); // Removes ownership, keeps the entry.
    output(&mut m, "host", 800, &a, "legacy stream");
    m.apply_eval_completion(
        "host".into(),
        800,
        &a,
        29,
        vec![ReplFrame::Stderr {
            text: "legacy terminal".into(),
        }],
    );
    assert_eq!(m.entries()[0].frames.len(), 2);
    assert_eq!(m.entries()[0].elapsed_ms, 29);
}

#[test]
fn terminal_replies_append_to_streamed_output() {
    for parked in [false, true] {
        for lost in [false, true] {
            let mut m = ReplModel::default();
            let (a, b) = (key("host", "a"), key("host", "b"));
            let id = submit(&mut m, &a, "eval");
            output(&mut m, "host", id, &a, "already shown");
            let active = if parked {
                switch(&mut m, &a, &b);
                &b
            } else {
                &a
            };
            let terminal = if lost {
                ReplFrame::Error {
                    message: "link lost".into(),
                    stacktrace: vec![],
                }
            } else {
                ReplFrame::Stdout {
                    text: "terminal".into(),
                }
            };
            m.apply_eval_completion("host".into(), id, active, 5, vec![terminal]);
            if parked {
                m.restore_row(&a);
            }
            assert_eq!(m.entries()[0].frames.len(), 2);
            assert!(texts(m.entries())[0].contains("already shown"));
            assert!(!m.entries()[0].in_flight);
        }
    }
}

#[test]
fn a_returning_host_reopens_only_its_named_closed_runs() {
    let mut m = ReplModel::default();
    let (a, b, c) = (key("one", "a"), key("one", "b"), key("two", "a"));
    let id = submit(&mut m, &a, "a");
    complete(&mut m, &a, id);
    let unnamed = submit(&mut m, &a, "unnamed");
    complete(&mut m, &a, unnamed);
    switch(&mut m, &a, &b);
    let bid = submit(&mut m, &b, "b");
    complete(&mut m, &b, bid);
    switch(&mut m, &b, &c);
    // Named runs on another host stay closed, including an equal id.
    announce(&mut m, &c, &c, id);
    complete(&mut m, &c, id);
    announce(&mut m, &c, &c, 800);
    complete(&mut m, &c, 800);
    switch(&mut m, &c, &a);
    assert!(
        m.reopen_runs(&"one".into(), &a, vec![id, bid, 800, 999])
            .redraw
    );
    assert_eq!(m.in_flight_ids(), [id]);
    assert!(!m.entries()[1].in_flight);
    assert!(m.parked_entries(&b).unwrap()[0].in_flight);
    assert!(m
        .parked_entries(&c)
        .unwrap()
        .iter()
        .all(|entry| !entry.in_flight));
    assert!(
        matches!(&m.entries()[0].frames[0], ReplFrame::Stderr { text } if text.contains("is still running"))
    );
    assert_eq!(m.entries().len(), 2);
    assert_eq!(interrupt(&mut m, &a, true).0, [id]);
    output(&mut m, "one", bid, &a, "back to b");
    done(&mut m, "one", bid, &a);
    assert_eq!(m.parked_entries(&b).unwrap()[0].frames.len(), 2);
    assert!(!m.parked_entries(&b).unwrap()[0].in_flight);
}

#[test]
fn lifecycle_frames_never_enter_an_eval_log() {
    let mut m = ReplModel::default();
    let mut directory = RowDirectory::default();
    let (a, b) = (key("one", "a"), key("two", "b"));
    let row = ws_info("b", "attachment");
    let canonical = row.workspace_id.clone();
    let update = directory.prepare_replace("two".into(), vec![row]);
    directory.commit_replace(update);
    announce(&mut m, &a, &a, 40);
    switch(&mut m, &a, &b);
    announce(&mut m, &b, &b, 40);
    switch(&mut m, &b, &a);
    for hint in [Some(canonical.as_str()), Some("b")] {
        let translated = directory.lifecycle_key("two", hint, Some("a"));
        let effects = m.apply_frame(
            "two".into(),
            40,
            &a,
            translated,
            ReplFrame::Lifecycle {
                state: "starting".into(),
            },
        );
        assert_eq!(effects.listing, Some("two".into()));
        assert!(effects.redraw);
        assert!(m.starting(&b, None));
    }
    let fallback = directory.lifecycle_key("two", None, Some("a"));
    m.apply_frame(
        "two".into(),
        40,
        &a,
        fallback,
        ReplFrame::Lifecycle {
            state: "starting".into(),
        },
    );
    assert!(m.starting(&key("two", "<default>"), Some("a")));
    assert!(m.entries()[0].frames.is_empty());
    assert!(m.parked_entries(&b).unwrap()[0].frames.is_empty());
    output(&mut m, "two", 40, &a, "still routed");
    assert_eq!(m.parked_entries(&b).unwrap()[0].frames.len(), 1);
}

#[test]
fn listing_catchup_is_nonempty_and_preserves_unmentioned_lifecycle() {
    let mut m = ReplModel::default();
    let mut directory = RowDirectory::default();
    let mut presentation = RowPresentation::default();
    let a = key("host", "a");
    let b = key("host", "b");
    for row in [&a, &b] {
        m.apply_frame(
            "host".into(),
            0,
            &a,
            row.clone(),
            ReplFrame::Lifecycle {
                state: "starting".into(),
            },
        );
    }
    for state in ["", "ready"] {
        let mut row = ws_info("a", "attachment");
        row.repl_state = state.into();
        let update = directory.prepare_replace("host".into(), vec![row]);
        directory.commit_replace(update);
        let e = presentation.refresh(
            &directory,
            &["host".into()],
            "host",
            Some("a"),
            Default::default(),
            std::time::Instant::now,
        );
        assert_eq!(e.repl_catchup.is_empty(), state.is_empty());
        m.apply_listing_catchup(e.repl_catchup);
        assert_eq!(m.starting(&a, None), state.is_empty());
        assert!(m.starting(&b, None));
    }
    let mut other = ws_info("c", "other");
    other.repl_state = "starting".into();
    let update = directory.prepare_replace("host".into(), vec![other]);
    directory.commit_replace(update);
    let e = presentation.refresh(
        &directory,
        &["host".into()],
        "host",
        Some("c"),
        Default::default(),
        std::time::Instant::now,
    );
    m.apply_listing_catchup(e.repl_catchup);
    assert!(!m.starting(&a, None));
    assert!(m.starting(&b, None));
    assert!(m.starting(&key("host", "c"), None));
}

#[test]
fn default_migration_preserves_collision_and_routing_rules() {
    let mut m = ReplModel::default();
    let (raw, default, other, live) = (
        key("one", "slug"),
        key("one", "<default>"),
        key("two", "slug"),
        key("one", "away"),
    );
    announce(&mut m, &default, &default, 1);
    m.edit_draft(DraftEdit::Replace("destination wins".into()));
    switch(&mut m, &default, &raw);
    // The window counter can share an external id; local registration replaces its route.
    let id = submit(&mut m, &raw, "raw");
    assert_eq!(id, 1);
    m.edit_draft(DraftEdit::Replace("raw draft".into()));
    switch(&mut m, &raw, &other);
    announce(&mut m, &other, &other, id);
    m.edit_draft(DraftEdit::Replace("other host".into()));
    switch(&mut m, &other, &live);
    m.apply_frame(
        "one".into(),
        0,
        &live,
        raw.clone(),
        ReplFrame::Lifecycle {
            state: "starting".into(),
        },
    );
    m.migrate_default_key(&raw, default.clone());
    assert!(!m.restore_row(&raw).0);
    assert!(m.restore_row(&default).0);
    assert_eq!(m.input(), "destination wins");
    m.restore_row(&live);
    output(&mut m, "one", id, &live, "migrated");
    output(&mut m, "two", id, &live, "other");
    assert_eq!(m.parked_entries(&default).unwrap()[0].frames.len(), 1);
    assert_eq!(m.parked_entries(&other).unwrap()[0].frames.len(), 1);
    assert!(m.starting(&default, Some("slug")));
    assert!(m.starting(&raw, None));
    assert!(!m.starting(&default, None));
    // A late pre-normalization reply can create another raw slot: migrate on every refresh.
    m.restore_row(&raw);
    m.edit_draft(DraftEdit::Replace("late".into()));
    m.capture_row(raw.clone());
    m.migrate_default_key(&raw, default.clone());
    m.restore_row(&default);
    assert_eq!(m.input(), "destination wins");
    assert!(m.restore_row(&other).0);
    assert_eq!(m.input(), "other host");
}

#[test]
fn the_repl_follows_the_selected_workspace() {
    let mut m = ReplModel::default();
    for (host, slug, scroll) in [
        ("one", "same", 7),
        ("two", "same", 13),
        ("two", "<default>", 23),
    ] {
        let row = key(host, slug);
        m.restore_row(&row);
        let p = begin(&mut m, &row, "submitted");
        assert_eq!(p.host, host);
        assert!(
            matches!(p.request, OutgoingReq::ReplEval { workspace_id, .. } if workspace_id == target(&row).workspace_id)
        );
        m.edit_draft(DraftEdit::Replace(format!("{host}/{slug}")));
        m.scroll_rows(scroll);
        m.capture_row(row);
    }
    for (host, slug, scroll) in [
        ("one", "same", 7),
        ("two", "same", 13),
        ("two", "<default>", 23),
    ] {
        assert!(m.restore_row(&key(host, slug)).0);
        assert_eq!(m.input(), format!("{host}/{slug}"));
        assert_eq!(m.scroll(), scroll);
    }
}

#[test]
fn history_restores_the_original_saved_line() {
    let mut m = ReplModel::default();
    let (a, b) = (key("host", "a"), key("host", "b"));
    let first = submit(&mut m, &a, "first");
    complete(&mut m, &a, first);
    announce(&mut m, &a, &a, 700);
    complete(&mut m, &a, 700); // Completed session code is empty, but is a candidate.
    let last = submit(&mut m, &a, "last");
    complete(&mut m, &a, last);
    submit(&mut m, &a, "still running");
    m.edit_draft(DraftEdit::Replace("original draft".into()));
    assert!(m.history_back());
    assert_eq!(m.input(), "last");
    m.edit_draft(DraftEdit::Typed(" edited"));
    assert!(m.history_back());
    assert_eq!(m.input(), "");
    switch(&mut m, &a, &b);
    m.restore_row(&a);
    assert!(m.history_back());
    assert_eq!(m.input(), "first");
    assert!(!m.history_back());
    assert!(m.history_forward());
    assert_eq!(m.input(), "");
    assert!(m.history_forward());
    assert_eq!(m.input(), "last");
    assert!(m.history_forward());
    assert_eq!(m.input(), "original draft");
    assert!(!m.history_forward());
    // Whitespace consumes the draft, retaining the current walk and its original saved line.
    assert!(m.history_back());
    m.edit_draft(DraftEdit::Replace(" ".into()));
    assert!(m.begin_eval(target(&a)).is_none());
    assert!(m.history_forward());
    assert_eq!(m.input(), "original draft");
}

#[test]
fn interrupt_memory_changes_only_after_successful_send() {
    let mut m = ReplModel::default();
    let (a, b) = (key("host", "a"), key("host", "b"));
    let id = submit(&mut m, &a, "local");
    announce(&mut m, &a, &a, 800);
    assert_eq!(interrupt(&mut m, &a, false), (vec![id, 800], false));
    assert_eq!(interrupt(&mut m, &a, true), (vec![id, 800], false));
    assert_eq!(interrupt(&mut m, &a, false), (vec![id, 800], true));
    switch(&mut m, &a, &b);
    m.restore_row(&a);
    assert_eq!(interrupt(&mut m, &a, true), (vec![id, 800], true));
    done(&mut m, "host", id, &a);
    assert_eq!(interrupt(&mut m, &a, true), (vec![800], false));
    done(&mut m, "host", 800, &a);
    m.edit_draft(DraftEdit::Typed("]"));
    m.edit_draft(DraftEdit::Typed("draft"));
    m.scroll_rows(10);
    assert!(m.prepare_interrupt(target(&a)).is_none());
    m.complete_interrupt(None, false);
    assert_eq!(m.input(), "");
    assert!(!m.pkg_mode());
    assert_eq!(m.scroll(), 0);
    m.reopen_runs(&"host".into(), &a, vec![800]);
    assert_eq!(
        interrupt(&mut m, &a, true),
        (vec![800], true),
        "empty interrupt did not erase memory"
    );
}

#[test]
fn pkg_mode_and_entry_prompts_belong_to_their_rows() {
    let mut m = ReplModel::default();
    let (a, b) = (key("host", "a"), key("host", "b"));
    m.edit_draft(DraftEdit::Typed("]"));
    assert!(m.pkg_mode());
    assert_eq!(m.input(), "");
    let p = begin(&mut m, &a, "status");
    assert!(matches!(p.request, OutgoingReq::ReplEval { mode: Some(mode), .. } if mode == "pkg"));
    assert!(m.entries()[0].pkg_mode);
    switch(&mut m, &a, &b);
    assert!(!m.pkg_mode());
    let p = begin(&mut m, &b, "] literal");
    assert!(matches!(
        p.request,
        OutgoingReq::ReplEval { mode: None, .. }
    ));
    switch(&mut m, &b, &a);
    assert!(m.pkg_mode());
    m.edit_draft(DraftEdit::Backspace);
    assert!(!m.pkg_mode());
    assert!(
        m.entries()[0].pkg_mode,
        "historical prompts keep their submitted mode"
    );
    m.restore_row(&b);
    assert!(!m.entries()[0].pkg_mode);
}

#[test]
fn run_file_acceptance_waits_for_done() {
    for fresh in [false, true] {
        let mut m = ReplModel::default();
        let (a, b) = (key("host", "a"), key("host", "b"));
        let p = m.begin_file_run(target(&a), "folder/file.jl".into(), fresh);
        let id = p.submission.eval_id();
        assert!(
            matches!(&p.request, OutgoingReq::ReplRunFile { path, fresh: f, workspace_id: Some(ws), .. } if path == "folder/file.jl" && *f == fresh && ws == "a")
        );
        let sent = m.complete_submission(p.submission, Ok(()));
        assert!(sent.status.unwrap().contains(if fresh {
            "resetting REPL"
        } else {
            "existing repl"
        }));
        switch(&mut m, &a, &b);
        let e = m.apply_run_file_reply("host".into(), id, &b, Ok(info(id, fresh, 0, vec![])));
        assert_eq!(
            e.status.as_deref(),
            Some(if fresh {
                "running 'file.jl' (fresh — project: project)…"
            } else {
                "running 'file.jl' (existing repl)…"
            })
        );
        assert!(m.parked_entries(&a).unwrap()[0].in_flight);
        output(&mut m, "host", id, &b, "stream");
        let e = m.apply_frame(
            "host".into(),
            id,
            &b,
            b.clone(),
            ReplFrame::Done {
                eval_id: id,
                elapsed_ms: 83,
            },
        );
        assert_eq!(
            e.status.as_deref(),
            Some(if fresh {
                "ran 'file.jl' (fresh — project: project, 83ms)"
            } else {
                "ran 'file.jl' (existing repl, 83ms)"
            })
        );
        let row = m.parked_entries(&a).unwrap();
        assert!(!row[0].in_flight);
        assert_eq!(row[0].elapsed_ms, 83);
        assert_eq!(row[0].frames.len(), 1);
        let e = m.apply_frame(
            "host".into(),
            id,
            &b,
            b.clone(),
            ReplFrame::Done {
                eval_id: id,
                elapsed_ms: 99,
            },
        );
        assert!(e.status.is_none(), "Done consumed display information");
        // Legacy synchronous and error replies append to existing streamed output, too.
        for error in [false, true] {
            m.restore_row(&a);
            let p = m.begin_file_run(target(&a), "folder/file.jl".into(), false);
            let id = p.submission.eval_id();
            output(&mut m, "host", id, &a, "early");
            switch(&mut m, &a, &b);
            let result = if error {
                Err("refused".into())
            } else {
                Ok(info(
                    id,
                    fresh,
                    6,
                    vec![ReplFrame::Stdout {
                        text: "sync".into(),
                    }],
                ))
            };
            let e = m.apply_run_file_reply("host".into(), id, &b, result);
            assert!(e
                .status
                .unwrap()
                .contains(if error { "failed · refused" } else { "6ms" }));
            let row = m.parked_entries(&a).unwrap();
            assert_eq!(row.last().unwrap().frames.len(), 2);
            assert!(!row.last().unwrap().in_flight);
        }
        m.restore_row(&a);
        let p = m.begin_file_run(target(&a), "file.jl".into(), false);
        let e = m.complete_submission(p.submission, Err("closed".into()));
        assert_eq!(
            e.status.as_deref(),
            Some("repl.run_file 'file.jl' failed · channel closed")
        );
        assert!(!m.entries().last().unwrap().in_flight);
    }
}

#[test]
fn clear_and_fresh_run_keep_their_distinct_reset_rules() {
    let mut m = ReplModel::default();
    let a = key("host", "a");
    let old = submit(&mut m, &a, "history");
    complete(&mut m, &a, old);
    let running = submit(&mut m, &a, "running");
    interrupt(&mut m, &a, true);
    m.edit_draft(DraftEdit::Typed("]"));
    m.edit_draft(DraftEdit::Replace("saved draft".into()));
    m.history_back();
    m.edit_draft(DraftEdit::Replace("editing history".into()));
    m.scroll_rows(12);
    let effects = m.clear_scrollback();
    assert!(effects.clear_images);
    assert!(effects.redraw);
    assert!(!effects.reset_build_anchor);
    assert_eq!(effects.status.as_deref(), Some("repl · scrollback cleared"));
    assert!(m.entries().is_empty());
    assert_eq!(m.input(), "editing history");
    assert!(m.pkg_mode());
    assert_eq!(m.scroll(), 0);
    assert!(m.history_forward());
    assert_eq!(m.input(), "saved draft");
    // Clear leaves window interrupt memory intact (same external id can still be announced).
    complete(&mut m, &a, running);
    announce(&mut m, &a, &a, running);
    assert_eq!(interrupt(&mut m, &a, true), (vec![running], true));
    let id = submit(&mut m, &a, "more history");
    complete(&mut m, &a, id);
    m.edit_draft(DraftEdit::Replace("fresh saved draft".into()));
    m.history_back();
    m.edit_draft(DraftEdit::Replace("fresh draft stays".into()));
    m.scroll_rows(21);
    let p = m.begin_file_run(target(&a), "file.jl".into(), true);
    assert_eq!(m.input(), "fresh draft stays");
    assert!(!m.pkg_mode());
    assert_eq!(m.scroll(), 0);
    assert_eq!(m.entries().len(), 1);
    assert!(!m.entries()[0].pkg_mode);
    assert!(!m.history_forward());
    let effects = m.complete_submission(p.submission, Ok(()));
    assert!(!effects.clear_images);
    let fresh_id = m.entries()[0].eval_id;
    done(&mut m, "host", fresh_id, &a);
    complete(&mut m, &a, running);
    announce(&mut m, &a, &a, running);
    assert_eq!(interrupt(&mut m, &a, true), (vec![running], true));
    assert_eq!(fresh_id, id + 1);
}

#[test]
fn scroll_operations_preserve_tail_direction_and_row_state() {
    let mut m = ReplModel::default();
    let (a, b) = (key("host", "a"), key("host", "b"));
    m.scroll_rows(12);
    assert_eq!(m.scroll(), 12);
    m.scroll_rows(-4);
    assert_eq!(m.scroll(), 8);
    m.scroll_line(true);
    assert_eq!(m.scroll(), 9);
    m.scroll_line(false);
    assert_eq!(m.scroll(), 8);
    m.commit_rendered_scroll(5);
    switch(&mut m, &a, &b);
    assert_eq!(m.scroll(), 0);
    m.scroll_rows(3);
    switch(&mut m, &b, &a);
    assert_eq!(m.scroll(), 5);
    m.edit_draft(DraftEdit::Typed("x"));
    assert_eq!(m.scroll(), 0);
    m.scroll_rows(-99);
    assert_eq!(m.scroll(), 0);
    m.commit_rendered_scroll(u16::MAX);
    m.scroll_line(true);
    assert_eq!(m.scroll(), u16::MAX);
    m.scroll_rows(1);
    assert_eq!(
        m.scroll(),
        0,
        "page/wheel conversion wraps, while line steps saturate"
    );
    m.scroll_line(false);
    assert_eq!(m.scroll(), 0);
    m.restore_row(&b);
    assert_eq!(m.scroll(), 3);
    m.edit_draft(DraftEdit::Paste("\r\npaste\rline"));
    assert_eq!(m.scroll(), 0);
    assert_eq!(m.input(), "\npaste\nline");
    m.edit_draft(DraftEdit::Newline);
    m.edit_draft(DraftEdit::Space);
    m.edit_draft(DraftEdit::Backspace);
    assert_eq!(m.input(), "\npaste\nline\n");
    m.edit_draft(DraftEdit::Typed("\ttext\n"));
    assert!(m.input().ends_with("\ntext"));
}

#[test]
fn browser_frames_return_actions_without_log_or_route_mutation() {
    let mut m = ReplModel::default();
    let (a, b) = (key("host", "a"), key("host", "b"));
    let id = submit(&mut m, &a, "figure");
    switch(&mut m, &a, &b);
    let e = m.apply_frame(
        "host".into(),
        id,
        &b,
        b.clone(),
        ReplFrame::Browser {
            url: "http://localhost/figure".into(),
            open: false,
            fe: Some("frontend".into()),
        },
    );
    let action = e.browser.unwrap();
    assert_eq!(action.host, "host");
    assert_eq!(action.url, "http://localhost/figure");
    assert!(!action.open);
    assert_eq!(action.fe.as_deref(), Some("frontend"));
    assert!(e.redraw);
    assert!(e.status.is_none());
    assert!(e.listing.is_none());
    assert!(m.parked_entries(&a).unwrap()[0].frames.is_empty());
    assert!(m.parked_entries(&a).unwrap()[0].in_flight);
    assert!(m.entries().is_empty());
    output(&mut m, "host", id, &b, "still routed");
    done(&mut m, "host", id, &b);
    assert_eq!(m.parked_entries(&a).unwrap()[0].frames.len(), 1);
    assert!(!m.parked_entries(&a).unwrap()[0].in_flight);
}

#[test]
fn destroy_cleanup_forgets_only_the_named_parked_row() {
    let mut m = ReplModel::default();
    let (a, b, c) = (key("one", "same"), key("two", "same"), key("one", "away"));
    let aid = submit(&mut m, &a, "a");
    switch(&mut m, &a, &b);
    let bid = submit(&mut m, &b, "b");
    m.edit_draft(DraftEdit::Replace("b draft".into()));
    switch(&mut m, &b, &c);
    m.apply_frame(
        "one".into(),
        0,
        &c,
        a.clone(),
        ReplFrame::Lifecycle {
            state: "starting".into(),
        },
    );
    m.forget_destroyed_row(&a);
    assert!(m.parked_entries(&a).is_none());
    assert!(m.parked_entries(&b).is_some());
    assert!(!m.restore_row(&a).0);
    // Retained route of the forgotten row still forbids live fallback for that id.
    announce(&mut m, &a, &a, aid); // ignored: the forgotten row's route is retained.
    announce(&mut m, &c, &a, 999); // Started cannot create an unvisited parked destination.
    assert!(m.parked_entries(&c).is_none());
    assert!(m.entries().is_empty());
    output(&mut m, "two", bid, &a, "retained route");
    assert_eq!(m.parked_entries(&b).unwrap()[0].frames.len(), 1);
    assert!(m.starting(&a, None));
    assert!(m.restore_row(&b).0);
    assert_eq!(m.input(), "b draft");
    assert_eq!(submit(&mut m, &b, "next"), bid + 1);
}
