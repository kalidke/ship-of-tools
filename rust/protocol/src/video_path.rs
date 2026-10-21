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
#[path = "video_path_tests.rs"]
mod tests;
