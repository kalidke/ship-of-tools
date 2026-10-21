use super::*;

#[test]
fn servable_video_is_the_five_extensions_in_any_case() {
    for p in ["a.mp4", "A.MP4", "b.WebM", "c.mov", "d.MKV", "e.m4v"] {
        assert!(is_servable_video(Path::new(p)), "{p} should be servable");
    }
    for p in ["clip.avi", "song.mp3", "x.gif", "noext", "mp4"] {
        assert!(!is_servable_video(Path::new(p)), "{p} should not be servable");
    }
}

/// The Julia plugin matches a name that ends in a dotted suffix, a leading-dot name included; the server
/// serves and types exactly those names.
#[test]
fn dotfile_video_has_one_route_and_mime() {
    let video = [
        (".mp4", "video/mp4"),
        (".M4V", "video/mp4"),
        ("dir.d/.webm", "video/webm"),
        ("a.b.c.MOV", "video/quicktime"),
        ("/x/y/clip.Mkv", "video/x-matroska"),
    ];
    for (p, mime) in video {
        assert!(is_servable_video(Path::new(p)), "{p} should be servable");
        assert_eq!(content_type(Path::new(p)), mime, "{p}");
    }
    for p in [
        "mp4",
        "webm",
        "dir.mp4/clip",
        "clip.mp4.txt",
        "clip.mp4/",
        "",
        "clip.avi",
    ] {
        assert!(
            !is_servable_video(Path::new(p)),
            "{p} should not be servable"
        );
        assert!(!content_type(Path::new(p)).starts_with("video/"), "{p}");
    }
}
