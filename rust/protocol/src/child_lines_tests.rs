use super::{blocking_child_lines, ChildLines};

/// (what a child writes, the lines read), the same by both readers: an invalid byte, a character cut inside a
/// JSON string (its closing quote survives), `\r\n`, an empty line and a last line with no `\n`.
#[tokio::test]
async fn every_byte_a_child_writes_reads_as_a_line() {
    let cases: &[(&[u8], &[&str])] = &[
        (b"a\xffb\n", &["a\u{fffd}b"]),
        (b"{\"e\":\"\xe3\x81\"}\nnext\n", &["{\"e\":\"\u{fffd}\"}", "next"]),
        (b"crlf\r\n\nlast", &["crlf", "", "last"]),
        (b"", &[]),
    ];
    for &(bytes, want) in cases {
        let mut lines = ChildLines::new(bytes);
        let mut read = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            read.push(line);
        }
        assert_eq!(read, want, "{bytes:?}");
        let blocking: Vec<String> = blocking_child_lines(bytes).map(Result::unwrap).collect();
        assert_eq!(blocking, want, "blocking {bytes:?}");
    }
}
