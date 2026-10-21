use super::*;

#[cfg(target_os = "linux")]
const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

fn home(inbox: &str, cursor: Option<&str>) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("inbox")).unwrap();
    std::fs::create_dir_all(d.path().join("read")).unwrap();
    std::fs::write(d.path().join("inbox/a.jsonl"), inbox).unwrap();
    if let Some(c) = cursor {
        std::fs::write(d.path().join("read/a.cursor"), c).unwrap();
    }
    d
}

const MINE: &str = "{\"from\":\"b\",\"to\":\"a\",\"msg\":\"x\"}\n";

fn hashed(n: usize, inbox: &str) -> String {
    format!("{n} {}\n", line_hash(inbox.split('\n').nth(n - 1).unwrap().as_bytes()))
}

#[test]
fn scan_counts_past_the_cursor() {
    let inbox = MINE.repeat(3);
    let d = home(&inbox, Some(&hashed(1, &inbox)));
    assert_eq!(scan(d.path(), "a", 0), Scan { total: 3, unread: 2, fresh: 2 });
    // A wake at line 2 leaves one fresh line.
    assert_eq!(scan(d.path(), "a", 2), Scan { total: 3, unread: 2, fresh: 1 });
    // The inbox shrank past the last wake: every unread line is fresh.
    assert_eq!(scan(d.path(), "a", 9), Scan { total: 3, unread: 2, fresh: 2 });
}

#[test]
fn cksum_matches_the_real_one() {
    // Values from the system `cksum`.
    assert_eq!(cksum(b""), (4294967295, 0));
    assert_eq!(cksum(b"a"), (1220704766, 1));
    assert_eq!(cksum(b"ab"), (2072780115, 2));
    assert_eq!(cksum(MINE.trim_end().as_bytes()), (3902411541, 31));
    assert_eq!(line_hash(b"a\0\n"), "1220704766-1");
}

fn off(inbox: &str, cursor: Option<&str>) -> u64 {
    cursor_offset(home(inbox, cursor).path(), "a")
}

const TS: &str = "{\"to\":\"a\",\"ts\":\"2026-01-0";

fn ts_inbox(stamps: &[&str]) -> String {
    stamps.iter().map(|t| format!("{TS}{t}Z\"}}\n")).collect()
}

#[test]
fn offsets_by_form() {
    let ib = MINE.repeat(3);
    // Count and hash matching; mismatching (one back); one past the end
    // with a hash (the total); further past (0); bare count past, in range.
    assert_eq!(off(&ib, Some(&hashed(2, &ib))), 2);
    assert_eq!(off(&ib, Some("2 1-1")), 1);
    assert_eq!(off(&ib, Some(&format!("4 {}", line_hash(b"x")))), 3);
    assert_eq!(off(&ib, Some("4")), 0);
    assert_eq!(off(&ib, Some("5 1-1")), 0);
    assert_eq!(off(&ib, Some("9")), 0);
    assert_eq!(off(&ib, Some("2")), 2);
    assert_eq!(off(&ib, Some("0")), 0);
    // Empty or absent.
    assert_eq!(off(&ib, Some("")), 0);
    assert_eq!(off(&ib, None), 0);
    assert_eq!(off("", Some("2")), 0);
}

#[test]
fn timestamp_cursors() {
    let ib = ts_inbox(&["1T00:00:01", "1T00:00:02", "1T00:00:03"]);
    // Nothing newer: the total, so no unread and no wake storm.
    assert_eq!(off(&ib, Some("2026-01-02T00:00:00Z")), 3);
    // A newer line in the middle.
    assert_eq!(off(&ib, Some("2026-01-01T00:00:01Z")), 1);
    // An unparseable line before the boundary is counted, never a boundary.
    let torn = format!("{}not json\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
    assert_eq!(off(&torn, Some("2026-01-01T00:00:05Z")), 2);
    // A skewed earlier line: the FIRST greater line wins.
    let skew = ts_inbox(&["1T00:00:09", "1T00:00:01", "1T00:00:02"]);
    assert_eq!(off(&skew, Some("2026-01-01T00:00:05Z")), 0);
    // A torn tail is a segment too, but a count past the total is 0.
    let tail = format!("{}{{\"ts\":", ts_inbox(&["1T00:00:01"]));
    assert_eq!(off(&tail, Some("2026-01-02T00:00:00Z")), 0);
}

#[test]
fn the_wake_reads_a_timestamp_cursor_as_no_mail() {
    let ib = ts_inbox(&["1T00:00:01", "1T00:00:02"]);
    let d = home(&ib, Some("2026-01-02T00:00:00Z\n"));
    assert_eq!(scan(d.path(), "a", 0).unread, 0);
}

/// The shell is the spec: every fixture is run through the real
/// `sot_cursor_offset` and must give the same number. Linux only, like the
/// other tests that run the shell: it needs bash and jq on PATH.
#[cfg(target_os = "linux")]
#[test]
fn agrees_with_the_shell() {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm/lib/comm-lib.sh");
    let ib = MINE.repeat(3);
    let ts = ts_inbox(&["1T00:00:01", "1T00:00:02", "1T00:00:03"]);
    let torn = format!("{}not json\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
    let skew = ts_inbox(&["1T00:00:09", "1T00:00:01", "1T00:00:02"]);
    let tail = format!("{}{{\"ts\":", ts_inbox(&["1T00:00:01"]));
    let nonobj = format!("{}5\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
    let arr = "{\"ts\":[1]}\n{\"ts\":\"2026-01-01T00:00:09Z\"}\n".to_string();
    let nul = format!("{{\"to\":\"a\",\"m\":\"x\0y\"}}\n{MINE}");
    let strs = format!("{}\"x\"\ntrue\n[1]\nnull\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
    let h2 = hashed(2, &ib);
    let h4 = format!("4 {}", line_hash(b"x"));
    let hn = hashed(1, &nul);
    let cases: Vec<(&str, Option<&str>)> = vec![
        (&ib, Some(&h2)),
        (&ib, Some("2 1-1")),
        (&ib, Some(&h4)),
        (&ib, Some("4")),
        (&ib, Some("5 1-1")),
        (&ib, Some("2")),
        (&ib, Some("0")),
        (&ib, Some("3 ")),
        (&ib, Some("")),
        (&ib, None),
        ("", Some("2")),
        (&nul, Some(&hn)),
        (&ts, Some("2026-01-02T00:00:00Z")),
        (&ts, Some("2026-01-01T00:00:01Z")),
        (&torn, Some("2026-01-01T00:00:05Z")),
        (&skew, Some("2026-01-01T00:00:05Z")),
        (&tail, Some("2026-01-02T00:00:00Z")),
        (&nonobj, Some("2026-01-01T00:00:05Z")),
        (&arr, Some("2026-01-01T00:00:05Z")),
        (&ib, Some("oops")),
        (&strs, Some("2026-01-01T00:00:05Z")),
    ];
    for (inbox, cursor) in cases {
        let d = home(inbox, cursor);
        let ran = sot_log::test_wait::run_within(
            std::process::Command::new("bash")
                .arg("-c")
                .arg(r#"source "$1"; sot_cursor_offset a"#)
                .arg("bash")
                .arg(&lib)
                .env("COMM_HOME", d.path())
                .env("SOT_COMM_HOME", d.path()),
            BOUND,
        );
        assert!(ran.status.success(), "the shell failed on inbox {inbox:?} cursor {cursor:?}: {ran}");
        let shell: u64 = ran.stdout.trim().parse().expect("shell offset");
        assert_eq!(cursor_offset(d.path(), "a"), shell, "inbox {inbox:?} cursor {cursor:?}: {ran}");
    }
}

/// `sot_unread` is the shell's copy of `counts`: `<total> <unread>` for the
/// same inboxes and cursors, including every line `counts` refuses. They agree
/// on every line a product writer emits (jq -c, serde_json). A line that is
/// not strict JSON (invalid UTF-8, a byte-order mark, NaN, a number out of
/// range, nesting past 127) may be counted by the shell and not by the wake.
#[cfg(target_os = "linux")]
#[test]
fn unread_agrees_with_the_shell() {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm/lib/comm-lib.sh");
    let ib = MINE.repeat(3);
    let lines = [
        MINE,
        "{\"from\":\"a\",\"to\":\"a\",\"msg\":\"own\"}\n",
        "{\"from\":\"b\",\"to\":\"c\",\"msg\":\"other\"}\n",
        "{\"from\":\"b\",\"to\":5,\"msg\":\"num\"}\n",
        "{\"from\":\"b\",\"msg\":\"no to\"}\n",
        "{\"to\":\"a\",\"msg\":\"no from\"}\n",
        "{\"from\":5,\"to\":\"a\",\"msg\":\"from 5\"}\n",
        "{\"from\":\"b\",\"to\":\"\",\"msg\":\"broadcast\"}\n",
        "not json\n",
        "{\"to\":\"a\",\"m\":\"x\0y\"}\n",
        "[1]\n",
        "\"a\"\n",
    ];
    let mixed: String = lines.concat();
    let torn = format!("{mixed}{{\"from\":\"b\",\"to\":\"a\"");
    let mut cases: Vec<(String, Option<String>)> = vec![(ib.clone(), None), (ib.clone(), Some("1".into())), (ib.clone(), Some(hashed(2, &ib)))];
    for l in lines {
        cases.push((l.to_string(), None));
    }
    // A timestamp cursor: the first line is read, the second is not.
    cases.push((ts_inbox(&["1T00:00:01", "1T00:00:09"]), Some("2026-01-01T00:00:05Z".into())));
    for inbox in [&mixed, &torn] {
        cases.push((inbox.clone(), None));
        cases.push((inbox.clone(), Some("1".into())));
        cases.push((inbox.clone(), Some("0".into())));
        cases.push((inbox.clone(), Some(hashed(3, inbox))));
    }
    for (inbox, cursor) in cases {
        let d = home(&inbox, cursor.as_deref());
        let ran = sot_log::test_wait::run_within(
            std::process::Command::new("bash")
                .arg("-c")
                .arg(r#"source "$1"; sot_unread a"#)
                .arg("bash")
                .arg(&lib)
                .env("COMM_HOME", d.path())
                .env("SOT_COMM_HOME", d.path()),
            BOUND,
        );
        assert!(ran.status.success(), "the shell failed on inbox {inbox:?} cursor {cursor:?}: {ran}");
        let got = scan(d.path(), "a", 0);
        let want = format!("{} {}", got.total, got.unread);
        assert_eq!(ran.stdout.trim_end(), want, "inbox {inbox:?} cursor {cursor:?}: {ran}");
    }
}

#[test]
fn scan_counts_a_missing_cursor_from_zero() {
    let d = home(&MINE.repeat(2), None);
    assert_eq!(scan(d.path(), "a", 0).unread, 2);
}

#[test]
fn an_unterminated_tail_is_not_counted() {
    let d = home(&format!("{MINE}{}", MINE.trim_end()), None);
    assert_eq!(scan(d.path(), "a", 0), Scan { total: 1, unread: 1, fresh: 1 });
}

#[test]
fn own_and_foreign_lines_are_not_counted() {
    let inbox = format!(
        "{MINE}{{\"from\":\"a\",\"to\":\"a\",\"msg\":\"me\"}}\n{{\"from\":\"b\",\"to\":\"c\",\"msg\":\"no\"}}\nnot json\n"
    );
    let d = home(&inbox, None);
    assert_eq!(scan(d.path(), "a", 0), Scan { total: 4, unread: 1, fresh: 1 });
}
