// session_state.rs — backend writes its identity to
// `~/.config/sot/sessions/<slug>.toml` on startup so Sessions mode can
// adopt a backend launched directly from a shell.
//
// Per ADR 0013, the toml is mostly frontend-owned (nav_state, layout,
// bl_pane sections). The backend just stamps a `[backend]` block with
// what it knows: session_id, label, project_dir, socket_path, started_at,
// pid. Frontend merges its own sections on top.
//
// Writes are best-effort: failure to write the toml is a warn, not fatal
// — a backend that can't reach `~/.config/sot/` still serves clients
// fine. Atomic via temp+rename.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::paths;

/// Path to the per-backend toml for a given label — in the PER-HOST sessions
/// dir (`sessions-<host>/`, see `workspaces::state_host`): this file records
/// THIS machine's daemon identity, and on a shared-$HOME cohort an unsuffixed
/// path made every box's daemon fight over it (this was the straggler writer
/// that recreated the legacy dir right after the 2026-07-03 migration).
pub fn toml_path(label: &str) -> PathBuf {
    crate::workspaces::sessions_state_dir().join(format!("{}.toml", paths::slug(label)))
}

/// Append-or-replace the `[backend]` section of the per-label toml with
/// the running daemon's identity. Other sections (nav_state, layout, …)
/// the frontend may have written are preserved verbatim. Writes
/// atomically via temp+rename.
pub fn write_backend_identity(
    label: &str,
    session_id: &str,
    project_root: &Path,
    socket_path: Option<&Path>,
) -> Result<PathBuf> {
    let target = toml_path(label);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {parent:?}"))?;
    }

    let existing = std::fs::read_to_string(&target).ok().unwrap_or_default();
    let preserved = strip_backend_section(&existing);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let pid = std::process::id();

    let mut body = String::new();
    body.push_str("[backend]\n");
    body.push_str(&format!("session_id    = {}\n", toml_quote(session_id)));
    body.push_str(&format!("label         = {}\n", toml_quote(label)));
    body.push_str(&format!(
        "project_dir   = {}\n",
        toml_quote(&project_root.to_string_lossy())
    ));
    body.push_str(&format!(
        "tmux_session  = {}\n",
        toml_quote(&paths::tmux_session_name(label))
    ));
    if let Some(s) = socket_path {
        body.push_str(&format!(
            "socket_path   = {}\n",
            toml_quote(&s.to_string_lossy())
        ));
    }
    body.push_str(&format!("started       = {now}\n"));
    body.push_str(&format!("pid           = {pid}\n"));

    let final_text = if preserved.trim().is_empty() {
        body
    } else if preserved.ends_with('\n') {
        format!("{body}\n{preserved}")
    } else {
        format!("{body}\n{preserved}\n")
    };

    let tmp = target.with_extension("toml.tmp");
    std::fs::write(&tmp, final_text.as_bytes())
        .with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("rename {tmp:?} -> {target:?}"))?;
    Ok(target)
}

/// Returns the contents of `text` with the `[backend]` section removed,
/// preserving every other section verbatim. Sections start at `[name]`
/// lines and end at the next `[name]` line or EOF. The frontend-managed
/// sections (`[nav_state]`, `[layout]`, `[bl_pane]`) survive intact.
fn strip_backend_section(text: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.contains(']') {
            skipping = trimmed.starts_with("[backend]");
            if skipping {
                continue;
            }
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Minimal TOML string-quote: backslash-escape `\` and `"`, wrap in
/// double quotes. The values we write (paths, slugs, session_ids) don't
/// contain control chars, so this covers them.
fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_handles_simple_strings() {
        assert_eq!(toml_quote("foo"), r#""foo""#);
        assert_eq!(toml_quote("/abs/path"), r#""/abs/path""#);
    }

    #[test]
    fn quote_escapes_quotes_and_backslashes() {
        assert_eq!(toml_quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(toml_quote(r"a\b"), r#""a\\b""#);
    }

    #[test]
    fn strip_backend_removes_only_backend_block() {
        let input = "\
[backend]
session_id = \"old\"
pid = 1

[nav_state]
mode = \"files\"

[layout]
left_col_pct = 50
";
        let stripped = strip_backend_section(input);
        assert!(!stripped.contains("[backend]"));
        assert!(stripped.contains("[nav_state]"));
        assert!(stripped.contains("mode = \"files\""));
        assert!(stripped.contains("[layout]"));
        assert!(stripped.contains("left_col_pct = 50"));
    }

    #[test]
    fn strip_backend_returns_empty_when_only_backend() {
        let input = "[backend]\nsession_id = \"x\"\npid = 1\n";
        let stripped = strip_backend_section(input);
        assert!(stripped.trim().is_empty());
    }

    #[test]
    fn strip_backend_preserves_other_first_section() {
        let input = "[layout]\nleft_col_pct = 60\n\n[backend]\nsession_id = \"x\"\n";
        let stripped = strip_backend_section(input);
        assert!(!stripped.contains("[backend]"));
        assert!(stripped.contains("[layout]"));
        assert!(stripped.contains("left_col_pct = 60"));
    }

    #[test]
    fn write_backend_identity_round_trip() {
        // Hermetic: use a per-test XDG_CONFIG_HOME.
        let tmp = std::env::temp_dir().join(format!(
            "sot-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        struct Guard(Option<std::ffi::OsString>);
        impl Drop for Guard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
        }
        let _g = Guard(std::env::var_os("XDG_CONFIG_HOME"));
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let written = write_backend_identity(
            "MyPkg.jl",
            "sess-123",
            Path::new("/home/u/MyPkg.jl"),
            Some(Path::new("/run/user/1000/sot/sessions/mypkg_jl.sock")),
        )
        .unwrap();
        let text = std::fs::read_to_string(&written).unwrap();
        assert!(text.contains("[backend]"));
        assert!(text.contains("label         = \"MyPkg.jl\""));
        assert!(text.contains("session_id    = \"sess-123\""));
        assert!(text.contains("project_dir   = \"/home/u/MyPkg.jl\""));
        assert!(text.contains("tmux_session  = \"sot-be-mypkg_jl\""));

        // Now write a frontend section to simulate the frontend later
        // editing the file, then re-stamp the backend identity. The
        // frontend section must survive verbatim.
        std::fs::write(
            &written,
            format!("{text}\n[nav_state]\nmode = \"files\"\ncursor_path = \"src/lib.jl\"\n"),
        )
        .unwrap();
        let _ = write_backend_identity(
            "MyPkg.jl",
            "sess-456",
            Path::new("/home/u/MyPkg.jl"),
            None,
        )
        .unwrap();
        let text2 = std::fs::read_to_string(&written).unwrap();
        assert!(text2.contains("session_id    = \"sess-456\""));
        assert!(text2.contains("[nav_state]"));
        assert!(text2.contains("mode = \"files\""));
        assert!(text2.contains("cursor_path = \"src/lib.jl\""));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
