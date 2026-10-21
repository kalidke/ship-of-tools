use super::*;

/// The field defect this guards: a terminal-exit reason built from a
/// deep voyage path (`state_dir/voyages/<uuid>/<segment>`) pushed the
/// `(os error N)` clean off the end of the fixed-width record field
/// once `bounded_detail` truncated it. The OS error code must survive
/// even when the path is far longer than the whole budget, and the
/// message must still name the op and end with the path's TAIL, not
/// its head.
#[test]
#[cfg(windows)]
fn io_ctx_keeps_the_os_error_code_and_the_path_tail_under_a_very_long_path() {
    let deep = std::path::PathBuf::from(format!(
        r"C:\Users\somebody\AppData\Local\sot\state\sot\workspaces\{}\voyages\{}\{}",
        "ws-".to_string() + &"x".repeat(60),
        "11111111-2222-3333-4444-555555555555",
        "segment-recognizable-tail.sotseg.tmp",
    ));
    let os_err = std::io::Error::from_raw_os_error(206); // ERROR_FILENAME_EXCED_RANGE
    let wrapped = io_ctx(os_err, format_args!("MoveFileExW {deep:?} -> {deep:?}"));
    let msg = format!("{wrapped}");
    assert!(msg.len() <= super::ERROR_DETAIL_BOUND, "{} bytes: {msg}", msg.len());
    assert!(msg.starts_with("io: MoveFileExW (os error 206): "), "{msg}");
    assert!(msg.ends_with("segment-recognizable-tail.sotseg.tmp\""), "{msg}");
}

#[test]
#[cfg(windows)]
fn io_ctx_passes_a_short_message_through_unchanged_but_for_the_code() {
    let short = std::path::PathBuf::from(r"C:\a\b.tmp");
    let os_err = std::io::Error::from_raw_os_error(5); // ERROR_ACCESS_DENIED
    let wrapped = io_ctx(os_err, format_args!("FlushFileBuffers dir {short:?}"));
    let msg = format!("{wrapped}");
    assert!(msg.starts_with("io: FlushFileBuffers dir (os error 5): "), "{msg}");
    assert!(msg.ends_with(r#"b.tmp""#), "{msg}");
    // Nothing lost: the code and the whole (short) path both survive.
    assert!(msg.contains("a") && msg.contains("b.tmp"), "{msg}");
}

/// A full volume keeps its native code through the context wrapper:
/// the caller reads `raw_os_error`, not the text.
#[test]
#[cfg(windows)]
fn io_ctx_keeps_disk_full_codes() {
    for code in [112, 39, 1295] {
        let wrapped = io_ctx(std::io::Error::from_raw_os_error(code), format_args!("MoveFileExW {:?}", r"C:\a"));
        match wrapped {
            Error::Io(e) => assert_eq!(e.raw_os_error(), Some(code)),
            other => panic!("{other:?}"),
        }
    }
}

/// No path at all in `what` (e.g. `OpenProcessToken`): the separator
/// must not leave a dangling `": "` with nothing after it.
#[test]
#[cfg(windows)]
fn io_ctx_with_no_path_in_what_has_no_dangling_separator() {
    let os_err = std::io::Error::from_raw_os_error(1008);
    let wrapped = io_ctx(os_err, format_args!("OpenProcessToken"));
    let msg = format!("{wrapped}");
    assert_eq!(msg, "io: OpenProcessToken (os error 1008)");
}
