use super::*;

#[test]
fn register_video_mints_fresh_lowercase_hex_tokens() {
    let a = register_video(PathBuf::from("/mw09/a.mp4")).expect("token");
    let b = register_video(PathBuf::from("/mw09/b.mp4")).expect("token");
    for t in [&a, &b] {
        assert_eq!(t.len(), 32, "token {t}");
        assert!(t.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')), "token {t}");
    }
    assert_ne!(a, b);
}
