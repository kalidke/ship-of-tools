//! Video file classification: the one Rust decision of which names are videos and what MIME they are served as.

/// The dotted suffixes of a video and the MIME each is served as.
const VIDEO_SUFFIXES: [(&str, &str); 5] = [
    (".mp4", "video/mp4"),
    (".webm", "video/webm"),
    (".mov", "video/quicktime"),
    (".mkv", "video/x-matroska"),
    (".m4v", "video/mp4"),
];

/// The MIME of a video, or `None` for any other name. A name is a video when it ends in one of the five dotted
/// suffixes in any ASCII case, as `ShipToolsVideoFile`'s `matches` decides: a leading-dot name such as `.mp4`
/// counts, a bare `mp4` or a directory component named `x.mp4` does not.
pub fn video_mime(path: &str) -> Option<&'static str> {
    let name = path.as_bytes();
    VIDEO_SUFFIXES
        .iter()
        .find(|(suffix, _)| name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes()))
        .map(|&(_, mime)| mime)
}

#[cfg(test)]
mod tests {
    use super::video_mime;

    /// The corpus the `matches suffix corpus` testset of `julia/plugins/video-file/test/runtests.jl` runs
    /// through `ConceptExplorerCore.matches`, with the same booleans.
    #[test]
    fn the_suffix_corpus_is_the_julia_contract() {
        for suffix in ["mp4", "webm", "mov", "mkv", "m4v"] {
            let upper = suffix.to_uppercase();
            let first = format!("{}{}", &upper[..1], &suffix[1..]);
            for spelling in [suffix, upper.as_str(), first.as_str()] {
                for path in [
                    format!("clip.{spelling}"),
                    format!(".{spelling}"),
                    format!("many.dots.clip.{spelling}"),
                    format!("folder/.{spelling}"),
                ] {
                    assert!(video_mime(&path).is_some(), "{path}");
                }
            }
        }
        for path in ["", "noext", ".", ".txt", "clip.mp4.txt", "dir.mp4/clip.txt", "clip.mp4/", "mp4"] {
            assert_eq!(video_mime(path), None, "{path:?}");
        }
    }

    #[test]
    fn each_suffix_has_its_mime() {
        assert_eq!(video_mime("a.mp4"), Some("video/mp4"));
        assert_eq!(video_mime("a.m4v"), Some("video/mp4"));
        assert_eq!(video_mime("a.WEBM"), Some("video/webm"));
        assert_eq!(video_mime("a.mov"), Some("video/quicktime"));
        assert_eq!(video_mime("a.mkv"), Some("video/x-matroska"));
    }
}
