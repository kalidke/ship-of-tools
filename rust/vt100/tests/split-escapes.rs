use std::io::Read as _;

fn get_file_contents(name: &str) -> Vec<u8> {
    let mut file = std::fs::File::open(name).unwrap();
    let mut buf = vec![];
    file.read_to_end(&mut buf).unwrap();
    buf
}

/// Returns the rendered text and the full serialized state, so a split that
/// changes anything at all — including the grid that is not on screen —
/// shows up as an inequality. Upstream paired `contents` with the screen's
/// escape-sequence rendering here; the checkpoint sees strictly more.
fn write_to_parser(chunks: &mut [Vec<u8>]) -> (String, Vec<u8>) {
    let mut parser = vt100_ctt::Parser::new(37, 193, 0);
    for chunk in chunks.iter_mut() {
        parser.process(chunk);
    }
    (
        parser.screen().contents(),
        parser.screen().checkpoint().expect("checkpoint"),
    )
}

fn test_splits(filename: &str, limit: Option<usize>) {
    let bytes = get_file_contents(filename);
    let len = bytes.len();
    let expected = write_to_parser(&mut [bytes.clone()]);
    for i in 0..(len - 1) {
        if let Some(limit) = limit {
            if i > limit {
                break;
            }
        }
        let bytes_copy = bytes.clone();
        let (start, end) = bytes_copy.split_at(i);
        let mut chunks = vec![start.to_vec(), end.to_vec()];
        let got = write_to_parser(&mut chunks);
        assert!(
            got == expected,
            "failed to render {filename} when split at byte {i}"
        );
    }
}

#[test]
fn split_escapes_weechat() {
    test_splits("tests/data/weechat.typescript", Some(500));
}

#[test]
#[ignore]
fn split_escapes_weechat_full() {
    test_splits("tests/data/weechat.typescript", None);
}
