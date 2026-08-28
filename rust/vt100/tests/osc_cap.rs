//! The OSC accumulation buffer is capped at 1024 bytes, unconditionally.
//!
//! Upstream vte caps it only on the `no_std`/arrayvec arm — which was the
//! arm actually active before the vendoring (vte 0.13's DEFAULT features
//! select `no_std`). The vendored copy keeps `Vec` storage but must keep
//! the cap: the parser sits in front of an untrusted producer, and an OSC
//! that never terminates must not grow memory without bound.

struct TitleLen(Option<usize>);

impl vt100_ctt::Callbacks for TitleLen {
    fn set_window_title(&mut self, _: &mut vt100_ctt::Screen, title: &[u8]) {
        self.0 = Some(title.len());
    }
}

/// A 100 KB OSC title arrives truncated to the cap (1024 bytes of raw OSC
/// content, one of which is the leading "2" selector), and the parser is
/// healthy afterwards — later output still renders.
#[test]
fn oversized_osc_is_capped_not_accumulated() {
    let mut parser =
        vt100_ctt::Parser::new_with_callbacks(24, 80, 0, TitleLen(None));
    parser.process(b"\x1b]2;");
    parser.process(&[b'a'; 100_000]);
    parser.process(b"\x07after");
    assert_eq!(parser.screen().contents(), "after");
    assert!(parser.is_ground());
    assert_eq!(parser.callbacks().0, Some(1023));
}
