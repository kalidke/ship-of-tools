mod helpers;

#[test]
fn bel() {
    struct State {
        bel: usize,
    }

    impl vt100_ctt::Callbacks for State {
        fn audible_bell(&mut self, _: &mut vt100_ctt::Screen) {
            self.bel += 1;
        }
    }

    let mut parser =
        vt100_ctt::Parser::new_with_callbacks(24, 80, 0, State { bel: 0 });
    assert_eq!(parser.callbacks().bel, 0);

    // A bell is counted and draws nothing. Upstream witnessed "draws nothing"
    // with `contents_diff` against a clone of the previous screen; this fork
    // has no diff API, so the screen contents are the witness instead.
    parser.process(b"\x07");
    assert_eq!(parser.callbacks().bel, 1);
    assert_eq!(parser.screen().contents(), "");

    parser.process(b"\x07");
    assert_eq!(parser.callbacks().bel, 2);
    assert_eq!(parser.screen().contents(), "");

    parser.process(b"\x07\x07\x07");
    assert_eq!(parser.callbacks().bel, 5);
    assert_eq!(parser.screen().contents(), "");

    parser.process(b"foo");
    assert_eq!(parser.callbacks().bel, 5);
    assert_eq!(parser.screen().contents(), "foo");

    parser.process(b"ba\x07r");
    assert_eq!(parser.callbacks().bel, 6);
    assert_eq!(parser.screen().contents(), "foobar");
}
#[test]
fn bs() {
    helpers::fixture("bs");
}

#[test]
fn tab() {
    helpers::fixture("tab");
}

#[test]
fn lf() {
    helpers::fixture("lf");
}

#[test]
fn vt() {
    helpers::fixture("vt");
}

#[test]
fn ff() {
    helpers::fixture("ff");
}

#[test]
fn cr() {
    helpers::fixture("cr");
}
