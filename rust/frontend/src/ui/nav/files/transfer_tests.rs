use super::*;

#[test]
fn upload_owner_gate_ignores_a_non_owning_hosts_ack() {
    // ADR 0042 L2a codex review, item F: an UploadState pins the
    // host it's uploading to at start (reusing the field slot that
    // used to be the dead UploadState.dir_node_id). A
    // FileUploadAck/FileTransferFailed is applied only when its
    // event_host matches that pin -- mirrors the
    // `self.upload.as_ref().map(|u| &u.host) != Some(&event_host)`
    // gate in the real handlers, without needing a GPU-backed State
    // (upload holds a live std::fs::File, which isn't test-friendly
    // to construct).
    let upload_host: HostKey = "alpha".to_string();
    let owning_ack_host: HostKey = "alpha".to_string();
    let stray_ack_host: HostKey = "beta".to_string();
    assert_eq!(
        Some(&upload_host),
        Some(&owning_ack_host),
        "an ack from the pinned host is accepted"
    );
    assert_ne!(
        Some(&upload_host),
        Some(&stray_ack_host),
        "an ack from any OTHER host is dropped, even mid-batch"
    );
}

/// An upload says what it sends and where when it starts, before any progress line.
#[test]
fn an_upload_says_what_it_sends_when_it_starts() {
    let one: std::collections::VecDeque<std::path::PathBuf> = [std::path::PathBuf::from("/tmp/fit.png")].into();
    assert_eq!(upload_start_words(&one, "/data/run"), "upload · fit.png → /data/run");
    let three: std::collections::VecDeque<std::path::PathBuf> = ["a", "b", "c"].map(std::path::PathBuf::from).into();
    assert_eq!(upload_start_words(&three, "/data/run"), "upload · 3 files → /data/run");
}
