//! `$PREFIX/install.json`, schema 1: read the way every reader of it reads it (by key, one per line), written whole and
//! last in the shape earlier releases' readers take (scripts/CLAUDE.md, Installers: "The record last, whole, in its old
//! shape"). The writer here is the one an install uses on every system; the apply scripts rewrite fields in place.

use std::path::Path;

/// The string value of `key` in an install.json text, as the scripts' readers take it: the first `"key": "value"`, the
/// value up to the next quote. `None` when the key or its string is absent.
pub(super) fn string(text: &str, key: &str) -> Option<String> {
    let rest = after_key(text, key)?.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

/// The boolean value of `key`, `None` when it is absent or not a boolean.
pub(super) fn boolean(text: &str, key: &str) -> Option<bool> {
    let rest = after_key(text, key)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn after_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let quoted = format!("\"{key}\"");
    let mut from = 0;
    while let Some(at) = text[from..].find(&quoted) {
        let rest = text[from + at + quoted.len()..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            return Some(rest.trim_start());
        }
        from += at + quoted.len();
    }
    None
}

/// The record's text, or empty when there is none to read.
pub(super) fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// What an install records.
#[derive(Debug, Clone)]
pub(super) struct Record {
    pub prefix: String,
    pub config: String,
    pub service: String,
    pub version: String,
    pub commit: String,
    pub installed_at: String,
    pub hub: String,
    pub daemon: bool,
    pub frontend: bool,
    pub pinned: bool,
}

/// A JSON string's contents: backslash and quote escaped, so an unusual prefix or alias cannot make a record that
/// parses wrong.
fn json_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The record's text: one key per line, in schema 1's order. `pinned` is there only when the run named its target.
pub(super) fn text(r: &Record) -> String {
    let pin = if r.pinned { ",\n  \"pinned\": true" } else { "" };
    format!(
        "{{\n  \"schema\": 1,\n  \"prefix\": \"{}\",\n  \"config\": \"{}\",\n  \"service\": \"{}\",\n  \"version\": \"{}\",\n  \"tag\": \"v{}\",\n  \"commit\": \"{}\",\n  \"installed_at\": \"{}\",\n  \"hub\": \"{}\",\n  \"daemon\": {},\n  \"frontend\": {}{pin}\n}}\n",
        json_str(&r.prefix),
        json_str(&r.config),
        r.service,
        r.version,
        r.version,
        r.commit,
        r.installed_at,
        json_str(&r.hub),
        r.daemon,
        r.frontend,
    )
}

/// Write `text` to `path` whole: beside it under a name of this process's own, then renamed over it, so a reader sees
/// the old file or the new one and an interrupted write leaves the old one.
pub(super) fn write_whole(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.new.{}",
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        std::process::id()
    ));
    let written = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The text with its pin set or removed, every other byte kept: the record is additive, and the apply scripts rewrite it
/// field by field, so a pin is an edit and never a parse and rewrite.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows pins before it stages; Unix writes the pin with the record"))]
pub(super) fn with_pin(text: &str, pinned: bool) -> String {
    let mut out = text.to_string();
    while let Some(at) = out.find("\"pinned\"") {
        // The pin and the comma before it go: `,\s*"pinned"\s*:\s*(true|false)`.
        let start = out[..at].trim_end().strip_suffix(',').map_or(at, str::len);
        let rest = out[at + "\"pinned\"".len()..].trim_start();
        let Some(rest) = rest.strip_prefix(':').map(str::trim_start) else { break };
        let word = if rest.starts_with("true") { 4 } else if rest.starts_with("false") { 5 } else { break };
        let end = out.len() - rest.len() + word;
        out.replace_range(start..end, "");
    }
    if pinned {
        if let Some(close) = out.rfind('}') {
            let body = out[..close].trim_end().to_string();
            out = format!("{body},\n  \"pinned\": true\n}}{}", &out[close + 1..]);
        }
    }
    out
}

/// Set or remove the pin in the record at `path`. A record that is not there gets the pin alone when it is set: the run
/// writes the rest of it last. Returns whether the record now pins.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows pins before it stages; Unix writes the pin with the record"))]
pub(super) fn set_pin(path: &Path, prefix: &Path, pinned: bool) -> std::io::Result<bool> {
    let Ok(text) = std::fs::read_to_string(path) else {
        if pinned {
            let bare = format!("{{\"schema\": 1, \"prefix\": \"{}\", \"pinned\": true}}\n", json_str(&prefix.to_string_lossy()));
            write_whole(path, &bare)?;
        }
        return Ok(pinned);
    };
    let new = with_pin(&text, pinned);
    if new != text {
        write_whole(path, &new)?;
    }
    Ok(pinned)
}

/// The time now, as the record writes it: `YYYY-MM-DDTHH:MM:SSZ`, UTC.
pub(super) fn now_utc() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(i64::try_from(days).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The calendar date of a day count since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
