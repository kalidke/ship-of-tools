// download.rs — OS-independent local-side logic for the file-download
// feature (the `d` key in NavTree). Pairs with the backend `file.download`
// op (chunked trailing-blob streaming). This module owns the "where + under
// what name does a downloaded file land locally" decision; the destination
// directory comes from `Settings::download_dir()` and the transport /
// chunk-reassembly half is wired in transport.rs / gpu.rs once the backend
// op lands.

use std::path::{Path, PathBuf};

/// Given a target directory and a desired filename, return a path that does
/// not collide with an existing file. If `<dir>/<name>` is free, return it
/// unchanged; otherwise insert ` (1)`, ` (2)`, … before the extension
/// (`circles.png` → `circles (1).png`), matching the browser/Finder
/// convention. Non-destructive: never returns a path that already exists.
///
/// Existence is checked against the filesystem; the caller writes the file.
/// Bounded retry count so a pathological directory can't spin forever — past
/// that we fall back to a nanosecond suffix (effectively unique).
pub fn non_clobbering_path(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = split_filename(filename);
    for n in 1..=9999u32 {
        let candidate = dir.join(suffixed(stem, ext, &n.to_string()));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Pathological fallback: timestamp suffix, near-certainly unique.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(suffixed(stem, ext, &nanos.to_string()))
}

/// Build `"<stem> (<sfx>)[.<ext>]"`.
fn suffixed(stem: &str, ext: Option<&str>, sfx: &str) -> String {
    match ext {
        Some(ext) => format!("{stem} ({sfx}).{ext}"),
        None => format!("{stem} ({sfx})"),
    }
}

/// Split a filename into (stem, optional extension). The extension is the
/// text after the *last* dot, but only when that dot isn't the first char —
/// so dotfiles like `.bashrc` keep their whole name as the stem and suffix
/// as `.bashrc (1)`, not `.bashrc.1`. Extension is returned without the dot.
fn split_filename(filename: &str) -> (&str, Option<&str>) {
    match filename.rfind('.') {
        Some(idx) if idx > 0 => (&filename[..idx], Some(&filename[idx + 1..])),
        _ => (filename, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique throwaway dir under the OS temp dir.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("sot_dl_test_{tag}_{nanos}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(path: &Path) {
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn split_filename_handles_ext_dotfile_and_none() {
        assert_eq!(split_filename("circles.png"), ("circles", Some("png")));
        assert_eq!(split_filename("archive.tar.gz"), ("archive.tar", Some("gz")));
        assert_eq!(split_filename(".bashrc"), (".bashrc", None));
        assert_eq!(split_filename("Makefile"), ("Makefile", None));
    }

    #[test]
    fn free_name_returned_unchanged() {
        let dir = temp_dir("free");
        let got = non_clobbering_path(&dir, "circles.png");
        assert_eq!(got, dir.join("circles.png"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn collision_inserts_suffix_before_extension() {
        let dir = temp_dir("collide");
        touch(&dir.join("circles.png"));
        let got = non_clobbering_path(&dir, "circles.png");
        assert_eq!(got, dir.join("circles (1).png"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn multiple_collisions_increment() {
        let dir = temp_dir("incr");
        touch(&dir.join("circles.png"));
        touch(&dir.join("circles (1).png"));
        let got = non_clobbering_path(&dir, "circles.png");
        assert_eq!(got, dir.join("circles (2).png"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dotfile_and_extensionless_suffix_appends_whole_name() {
        let dir = temp_dir("dotfile");
        touch(&dir.join(".bashrc"));
        touch(&dir.join("Makefile"));
        assert_eq!(non_clobbering_path(&dir, ".bashrc"), dir.join(".bashrc (1)"));
        assert_eq!(non_clobbering_path(&dir, "Makefile"), dir.join("Makefile (1)"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
