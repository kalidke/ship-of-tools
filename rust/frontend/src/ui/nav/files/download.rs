// download.rs — OS-independent local-side logic for the file-download
// feature (the `d` key in NavTree). Pairs with the backend `file.download`
// op (chunked trailing-blob streaming). This module owns the "where + under
// what name does a downloaded file land locally" decision; the destination
// directory comes from `Settings::download_dir()`.

use std::path::{Path, PathBuf};

/// Given a target directory and a desired filename, return a path that does
/// not collide with an existing file. If `<dir>/<name>` is free, return it
/// unchanged; otherwise insert ` (1)`, ` (2)`, … before the extension
/// (`circles.png` → `circles (1).png`), matching the browser/Finder
/// convention. Non-destructive: never returns a path that already exists.
///
/// Existence is checked against the filesystem for the plain name and every
/// numbered candidate; the caller writes the file. Bounded retry count so a
/// pathological directory can't spin forever — past that we fall back to a
/// suffix that is unique by construction ([`sot_updater::unique::suffix`]) and
/// is NOT existence-checked, because nothing this process or any other has
/// produced can carry it.
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
    // Pathological fallback. This used to be a bare timestamp, called
    // "near-certainly unique" — but two downloads reaching here inside one
    // clock tick would pick the same name and one would overwrite the other,
    // in a function whose whole contract is "never returns a path that already
    // exists". Unique by construction instead; see `sot_updater::unique`.
    dir.join(suffixed(stem, ext, &sot_updater::unique::suffix()))
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
#[path = "download_tests.rs"]
mod tests;
