mod helpers;

#[test]
fn title_icon_name() {
    #[derive(Default)]
    struct Window {
        title: String,
        icon_name: String,
    }
    impl vt100_ctt::Callbacks for Window {
        fn set_window_icon_name(
            &mut self,
            _: &mut vt100_ctt::Screen,
            icon_name: &[u8],
        ) {
            self.icon_name =
                std::str::from_utf8(icon_name).unwrap().to_string();
        }
        fn set_window_title(&mut self, _: &mut vt100_ctt::Screen, title: &[u8]) {
            self.title = std::str::from_utf8(title).unwrap().to_string();
        }
    }

    let mut parser =
        vt100_ctt::Parser::new_with_callbacks(24, 80, 0, Window::default());
    assert_eq!(parser.callbacks().icon_name, "");
    assert_eq!(parser.callbacks().title, "");
    parser.process(b"\x1b]1;icon_name\x07");
    assert_eq!(parser.callbacks().icon_name, "icon_name");
    assert_eq!(parser.callbacks().title, "");
    parser.process(b"\x1b]2;title\x07");
    assert_eq!(parser.callbacks().icon_name, "icon_name");
    assert_eq!(parser.callbacks().title, "title");
    parser.process(b"\x1b]0;both\x07");
    assert_eq!(parser.callbacks().icon_name, "both");
    assert_eq!(parser.callbacks().title, "both");
}

#[test]
fn clipboard() {
    #[derive(Default)]
    struct Clipboard {
        clipboard: std::collections::HashMap<Vec<u8>, Vec<u8>>,
        pasted: Vec<Vec<u8>>,
    }
    impl vt100_ctt::Callbacks for Clipboard {
        fn copy_to_clipboard(
            &mut self,
            _: &mut vt100_ctt::Screen,
            ty: &[u8],
            data: &[u8],
        ) {
            self.clipboard.insert(ty.to_vec(), data.to_vec());
        }

        fn paste_from_clipboard(&mut self, _: &mut vt100_ctt::Screen, ty: &[u8]) {
            self.pasted.push(ty.to_vec());
        }

        fn unhandled_osc(&mut self, _: &mut vt100_ctt::Screen, params: &[&[u8]]) {
            panic!("unhandled osc: {params:?}");
        }
    }

    let mut parser =
        vt100_ctt::Parser::new_with_callbacks(24, 80, 0, Clipboard::default());
    assert!(parser.callbacks().clipboard.is_empty());
    assert!(parser.callbacks().pasted.is_empty());
    parser.process(b"\x1b]52;c;?\x07");
    assert!(parser.callbacks().clipboard.is_empty());
    assert_eq!(&parser.callbacks().pasted, &[b"c"]);
    parser.process(b"\x1b]52;c;abcdef==\x07");
    assert_eq!(parser.callbacks().clipboard.len(), 1);
    assert_eq!(
        parser.callbacks().clipboard.get(&b"c"[..]),
        Some(&b"abcdef==".to_vec())
    );
    assert_eq!(&parser.callbacks().pasted, &[b"c"]);
}

#[test]
fn unknown_osc() {
    helpers::fixture("unknown_osc");
}

// FORK: an OSC 52 copy longer than upstream's 1024-byte buffer arrives whole, in pieces as a terminal reads them; one
// past the bound is dropped whole, never delivered cut short, and the next copy is delivered.
#[test]
fn a_long_clipboard_copy_arrives_whole_and_an_overlong_one_not_at_all() {
    #[derive(Default)]
    struct Copies(Vec<Vec<u8>>);
    impl vt100_ctt::Callbacks for Copies {
        fn copy_to_clipboard(&mut self, _: &mut vt100_ctt::Screen, _ty: &[u8], data: &[u8]) {
            self.0.push(data.to_vec());
        }
    }
    let mut parser = vt100_ctt::Parser::new_with_callbacks(24, 80, 0, Copies::default());
    let long = "QUJD".repeat(25_000).into_bytes(); // 100,000 bytes of base64
    let mut seq = b"\x1b]52;c;".to_vec();
    seq.extend_from_slice(&long);
    seq.push(0x07);
    for piece in seq.chunks(4096) {
        parser.process(piece);
    }
    assert_eq!(parser.callbacks().0, vec![long], "the whole copy, not its first kilobyte");

    let mut overlong = b"\x1b]52;c;".to_vec();
    overlong.extend(std::iter::repeat(b'A').take(1 << 20));
    overlong.push(0x07);
    parser.process(&overlong);
    assert_eq!(parser.callbacks().0.len(), 1, "a copy past the bound is not delivered at all");

    parser.process(b"\x1b]52;c;aGk=\x07");
    assert_eq!(parser.callbacks().0.last().map(Vec::as_slice), Some(&b"aGk="[..]), "the next copy is");
    parser.process(b"after");
    assert!(parser.screen().contents().contains("after"), "the screen still takes text");
}
