//! Masking of page secrets in log output (ADR 0049, User isolation): `redact` and the writer that applies it.

use std::borrow::Cow;

/// `bytes` with the value after every `secret=` (up to the first `&`, `#`, whitespace, `"`, `'`, `<`, `>`, `)`, `\`,
/// or the end) and every maximal run of exactly 32 characters in `[0-9a-f]` replaced by `<redacted>`; every other
/// byte unchanged; borrowed when nothing matched.
pub fn redact(bytes: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut done = 0; // bytes[..done] has been copied to `out`
    let mut i = 0;
    while i < bytes.len() {
        let (masked, next) = if bytes[i..].starts_with(MARKER) {
            let start = i + MARKER.len();
            let end = bytes[start..].iter().position(|&b| ends_value(b)).map_or(bytes.len(), |n| start + n);
            (start..end, end)
        } else if is_hex(bytes[i]) {
            let end = bytes[i..].iter().position(|&b| !is_hex(b)).map_or(bytes.len(), |n| i + n);
            (if end - i == RUN { i..end } else { 0..0 }, end)
        } else {
            i += 1;
            continue;
        };
        if !masked.is_empty() {
            let out = out.get_or_insert_with(|| Vec::with_capacity(bytes.len()));
            out.extend_from_slice(&bytes[done..masked.start]);
            out.extend_from_slice(MASK);
            done = masked.end;
        }
        i = next;
    }
    match out {
        None => Cow::Borrowed(bytes),
        Some(mut out) => {
            out.extend_from_slice(&bytes[done..]);
            Cow::Owned(out)
        }
    }
}

const MARKER: &[u8] = b"secret=";
const MASK: &[u8] = b"<redacted>";
/// The length of a token `random_token` mints: 128 bits as lowercase hex.
const RUN: usize = 32;

fn is_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

fn ends_value(b: u8) -> bool {
    matches!(b, b'&' | b'#' | b'"' | b'\'' | b'<' | b'>' | b')' | b'\\') || b.is_ascii_whitespace()
}

/// A log writer that hands `redact(buf)` to the inner writer whole and reports all of `buf` written, so one call
/// masks one event.
pub struct RedactingWriter<W>(pub W);

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(&redact(buf))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const MARK: &str = "<redacted>";

    fn red(s: &str) -> String {
        String::from_utf8(redact(s.as_bytes()).into_owned()).unwrap()
    }

    /// A token as the daemon's `random_token` mints it: 16 bytes from the OS, 32 lowercase hex characters.
    fn minted() -> String {
        let mut buf = [0u8; 16];
        getrandom::fill(&mut buf).unwrap();
        buf.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// ADR 0049, User isolation: a secret an event carries never reaches the file, through the real fmt layer.
    #[test]
    fn a_logged_secret_is_masked_in_the_file() {
        let token = minted();
        let padding = "pad-zz ".repeat(700);
        assert!(padding.len() > 4096);
        let file = tempfile::tempfile().unwrap();
        let sink = file.try_clone().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || RedactingWriter(sink.try_clone().unwrap()))
            .finish();
        let _log = crate::test_log::install(subscriber);
        tracing::error!(
            token = %token,
            "open http://127.0.0.1:1234/edit?secret=Ab12Cd34&id=1 {padding}"
        );
        let mut written = String::new();
        let mut file = file;
        std::io::Seek::rewind(&mut file).unwrap();
        file.read_to_string(&mut written).unwrap();
        assert!(!written.contains(&token), "the token reached the file");
        assert!(!written.contains("Ab12Cd34"), "the secret reached the file");
        assert_eq!(written.matches(MARK).count(), 2, "{written}");
        assert!(written.contains(&padding), "the padding was lost");
    }

    #[test]
    fn only_a_run_of_exactly_32_lowercase_hex_characters_is_masked() {
        for n in [31usize, 33, 64] {
            let run = "a1".repeat(n).chars().take(n).collect::<String>();
            assert_eq!(red(&run), run, "{n} characters");
        }
        let run32: String = "0123456789abcdef".repeat(2);
        assert_eq!(red(&run32), MARK);
        assert_eq!(red(&format!("id={run32}.")), format!("id={MARK}."));
    }

    #[test]
    fn uppercase_hex_and_other_letters_bound_a_run() {
        let run32: String = "0123456789abcdef".repeat(2);
        assert_eq!(red(&format!("A{run32}B")), format!("A{MARK}B"));
        assert_eq!(red(&format!("{run32}g")), format!("{MARK}g"));
        let upper = run32.to_uppercase();
        assert_eq!(red(&upper), upper, "uppercase hex is not in the class");
    }

    #[test]
    fn the_value_after_secret_ends_at_each_delimiter() {
        for end in ["&id=1", "#frag", " tail", "\"tail", "'tail", "<tail", ">tail", ")tail", "\\tail", ""] {
            assert_eq!(red(&format!("a?secret=Ab12Cd34{end}")), format!("a?secret={MARK}{end}"), "{end:?}");
        }
    }

    #[test]
    fn two_secrets_on_one_line_are_both_masked() {
        assert_eq!(red("x?secret=one&y?secret=two z"), format!("x?secret={MARK}&y?secret={MARK} z"));
    }

    #[test]
    fn a_secret_marker_at_the_end_has_nothing_to_mask() {
        assert_eq!(red("tail secret="), "tail secret=");
        assert_eq!(red("secret=&x"), "secret=&x");
    }

    #[test]
    fn no_match_returns_the_input_borrowed() {
        let input = b"plain text with id=12 and a short hex deadbeef";
        assert!(matches!(redact(input), Cow::Borrowed(_)));
        assert!(matches!(redact(b""), Cow::Borrowed(_)));
    }

    #[test]
    fn one_write_call_masks_the_whole_buffer_and_reports_it_all_written() {
        use std::io::Write;
        let mut sink = Vec::new();
        let n = RedactingWriter(&mut sink).write(b"u?secret=abc\n").unwrap();
        assert_eq!(n, 13);
        assert_eq!(sink, format!("u?secret={MARK}\n").into_bytes());
    }
}
