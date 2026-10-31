//! The OSC accumulation buffer is capped, unconditionally: at 1 MiB, so an OSC 52 copy of a long text fits.
//!
//! Upstream vte caps it only on the `no_std`/arrayvec arm — which was the
//! arm actually active before the vendoring (vte 0.13's DEFAULT features
//! select `no_std`). The vendored copy keeps `Vec` storage but must keep
//! the cap: the parser sits in front of an untrusted producer, and an OSC
//! that never terminates must not grow memory without bound. An OSC that
//! outgrows the cap is dropped whole at its end, never dispatched cut short.

struct TitleLen(Option<usize>);

impl vt100_ctt::Callbacks for TitleLen {
    fn set_window_title(&mut self, _: &mut vt100_ctt::Screen, title: &[u8]) {
        self.0 = Some(title.len());
    }
}

/// A 100 KB OSC title arrives whole; a 2 MB one is dropped, never dispatched
/// cut at the cap; the parser is healthy afterwards — later output still
/// renders.
#[test]
fn oversized_osc_is_capped_not_accumulated() {
    let mut parser =
        vt100_ctt::Parser::new_with_callbacks(24, 80, 0, TitleLen(None));
    parser.process(b"\x1b]2;");
    parser.process(&[b'a'; 100_000]);
    parser.process(b"\x07");
    assert_eq!(parser.callbacks().0, Some(100_000));
    parser.callbacks_mut().0 = None;
    parser.process(b"\x1b]2;");
    parser.process(&vec![b'a'; 2_000_000]);
    parser.process(b"\x07after");
    assert_eq!(parser.screen().contents(), "after");
    assert!(parser.is_ground());
    assert_eq!(parser.callbacks().0, None, "an OSC past the cap is not dispatched");
}
