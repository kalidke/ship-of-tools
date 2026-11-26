use super::*;

#[test]
fn the_lists_are_disjoint_and_reasoned() {
    for (name, reason) in INLINE.iter().chain(POOLED) {
        assert!(!reason.is_empty(), "{name} has no reason");
    }
    for (name, _) in INLINE {
        assert!(POOLED.iter().all(|(o, _)| o != name), "{name} is in both lists");
    }
    assert_eq!(place(op::PING), Place::Inline);
    assert_eq!(place(op::WORKSPACE_LIST), Place::Inline);
    assert_eq!(place(op::QUARTO_OPEN), Place::Job);
    assert_eq!(place(op::PREVIEW_GET), Place::Pooled);
    assert_eq!(place("no.such"), Place::Job);
    // A chat request waits on Slack (the setup ops also on a file's write and a reload): each is a job, never in the loop.
    for name in [op::CHAT_POST, op::CHAT_POST_FILE, op::CHAT_DELETE_POST, op::CHAT_READ, op::CHAT_HISTORY, op::CHAT_RESOLVE, op::CHAT_CONNECT, op::CHAT_CHANNELS, op::CHAT_PAIR, op::CHAT_ROW_PAIR, op::CHAT_WORKSPACES] {
        assert_eq!(place(name), Place::Job, "{name}");
    }
    // A watch runs in the loop: its reply is written before the loop reads another chat event, and its snapshot is the
    // owner's, so it waits on admission and memory alone.
    for name in [op::CHAT_WATCH, op::CHAT_UNWATCH] {
        assert_eq!(place(name), Place::Inline, "{name}");
    }
    // Selections apply in the order sent: a later one replaces an earlier one.
    assert_eq!(place(op::SELECTION_SET), Place::Inline, "selection.set");
}
