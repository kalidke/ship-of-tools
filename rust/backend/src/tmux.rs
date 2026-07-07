// tmux.rs — wraps the `tmux` CLI as the backend-sessions registry per ADR
// 0013. Sessions mode in the frontend talks to these ops to enumerate live
// backends, spawn new ones, and live-tail panes.
//
// Why shell out instead of linking a libtmux: tmux's only stable interface
// is the CLI + the format strings (`-F`). There is no public C API. Forking
// `tmux ...` is one syscall and the parsing is line-oriented; on a backend
// that already supervises a Julia kernel and a node sidecar, one more
// child is rounding error.
//
// ADR 0010 originally had the backend daemon's own tmux CLI talk to the SAME
// server its hosting pane lives in (no `-S`/`-L`) — "shared server, one
// session per backend, plus `sot-home` and any non-Ship of Tools sessions
// the user has." Security review reversed that: every session this client
// creates/inspects now lives on a PRIVATE, per-user server instead
// (`paths::tmux_socket_path()`, `-S` on every invocation) — the shared
// default server's socket can end up reachable by another local user on a
// box where accounts share a GID (see that function's doc comment), which
// would otherwise let them attach straight into a live agent session.
// The sot-comm shell scripts (`comm/core/scripts/*.sh`) now resolve and pass
// the SAME `-S <socket>` via `sot_tmux_socket()` in comm-lib.sh (and an
// inline copy in codex-watch.sh, which doesn't source comm-lib.sh) — the
// compatibility break noted in an earlier revision of this comment is
// closed.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

/// Every invocation targets the private per-user socket
/// (`paths::tmux_socket_path()`) via `-S` — see the module doc comment.
#[derive(Debug, Clone, Default)]
pub struct TmuxClient {
    _private: (),
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub name: String,
    /// Unix epoch seconds.
    pub created: i64,
    pub attached: bool,
    pub windows: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneInfo {
    /// tmux pane id (e.g. `%23`). Stable for the pane's lifetime within the
    /// server; use this as the addressing target, not `<session>:w.p`.
    pub id: String,
    pub session: String,
    pub window_index: u32,
    pub pane_index: u32,
    pub title: String,
    pub command: String,
    pub pid: u32,
    pub width: u32,
    pub height: u32,
    pub active: bool,
}

impl TmuxClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// All sessions on the server. Returns an empty list (not an error) if
    /// no server is running — that's the legitimate "nothing yet" state at
    /// fresh boot, not a failure to enumerate.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let fmt = "#{session_name}|#{session_created}|#{session_attached}|#{session_windows}|#{session_width}|#{session_height}";
        let out = match self.run(&["list-sessions", "-F", fmt]) {
            Ok(o) => o,
            Err(e) => {
                // `list-sessions` exits 1 with "no server running on …" when
                // tmux has never been started; that's an empty list.
                let msg = e.to_string();
                if msg.contains("no server running") {
                    return Ok(Vec::new());
                }
                return Err(e);
            }
        };
        let text = String::from_utf8_lossy(&out);
        let mut sessions = Vec::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            sessions.push(parse_session_line(line).with_context(|| {
                format!("parse list-sessions line: {line:?}")
            })?);
        }
        Ok(sessions)
    }

    /// All panes in `session`. If `session` is `None`, lists all panes on
    /// the server. The frontend usually passes a specific session; the
    /// `None` form is for diagnostics.
    pub fn list_panes(&self, session: Option<&str>) -> Result<Vec<PaneInfo>> {
        let fmt = "#{pane_id}|#{session_name}|#{window_index}|#{pane_index}|#{pane_title}|#{pane_current_command}|#{pane_pid}|#{pane_width}|#{pane_height}|#{pane_active}";
        let mut args: Vec<&str> = vec!["list-panes", "-F", fmt];
        match session {
            Some(s) => {
                args.push("-t");
                args.push(s);
                // Without -s the target is interpreted as a pane id; -s
                // tells tmux it's a session and to list every window's panes.
                args.push("-s");
            }
            None => {
                args.push("-a");
            }
        }
        let out = match self.run(&args) {
            Ok(o) => o,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no server running") || msg.contains("can't find session") {
                    return Ok(Vec::new());
                }
                return Err(e);
            }
        };
        let text = String::from_utf8_lossy(&out);
        let mut panes = Vec::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            panes.push(parse_pane_line(line).with_context(|| {
                format!("parse list-panes line: {line:?}")
            })?);
        }
        Ok(panes)
    }

    /// Foreground command of `session`'s active pane — e.g. `claude` when an
    /// agent session already has claude running, `bash` at a shell prompt.
    /// This is the authoritative "is claude already running here?" signal:
    /// the frontend consults it (via `PtyOpenRes.pane_command`) before
    /// auto-starting ccb, so it never types the launcher into a live agent's
    /// prompt — the prompt-spam bug, where the FE's screen-scrape +
    /// in-memory `autostarted_sessions` heuristic mis-fired on an idle,
    /// long-lived session after an FE relaunch. Returns `None` if the session
    /// is gone or reports no (non-empty) active-pane command.
    pub fn active_pane_command(&self, session: &str) -> Option<String> {
        let panes = self.list_panes(Some(session)).ok()?;
        panes
            .into_iter()
            .find(|p| p.active)
            .map(|p| p.command)
            .filter(|c| !c.is_empty())
    }

    /// Spawn a new detached session. `command` runs as the session's first
    /// window; `cwd` is the working directory. The detached form (`-d`)
    /// never attaches the calling client — important because the backend
    /// is itself running inside a tmux client and we don't want recursive
    /// attach.
    pub fn create_session(
        &self,
        name: &str,
        command: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<()> {
        let mut args: Vec<String> = vec!["new-session".into(), "-d".into(), "-s".into(), name.into()];
        if let Some(p) = cwd {
            let s = p.to_str().context("cwd path must be UTF-8")?;
            args.push("-c".into());
            args.push(s.into());
        }
        if let Some(cmd) = command {
            args.push(cmd.into());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&arg_refs)?;
        Ok(())
    }

    pub fn kill_session(&self, name: &str) -> Result<()> {
        self.run(&["kill-session", "-t", name])?;
        Ok(())
    }

    /// Detach every client attached to `session`, leaving the session itself
    /// alive. `detach-client -s <session>` is tmux's contract for "the client
    /// goes away, the session stays" — used by the pty re-target / teardown
    /// path (`Pty::shutdown`) so a Sessions-mode switch-away releases the old
    /// session cleanly instead of letting the abrupt master-fd close race tmux
    /// into destroying the now-clientless session.
    ///
    /// Best-effort and quiet: a session that has no attached client (or is gone
    /// entirely after a server death) is a no-op as far as we care — we log at
    /// debug and swallow the error rather than propagate, because the only
    /// caller is a teardown that has nothing to recover.
    pub fn detach_session_clients(&self, session: &str) {
        if let Err(e) = self.run(&["detach-client", "-s", session]) {
            tracing::debug!(session, error = %e, "detach-client (teardown) — ignoring");
        }
    }

    /// Last `lines` rows of the pane's scrollback as plain text. Pass a
    /// negative line count via `-S -<lines>` — that's tmux's convention for
    /// "this far back from the bottom." We cap aggressively (default 200)
    /// so a frontend that polls live tail doesn't pay 50k-line transcripts.
    pub fn capture_pane(&self, pane: &str, lines: u32) -> Result<String> {
        let s_arg = format!("-{}", lines.max(1));
        let out = self.run(&["capture-pane", "-p", "-t", pane, "-S", &s_arg])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let socket = crate::paths::tmux_socket_path();
        // The socket FILE is tmux's to create; the containing DIRECTORY is
        // ours, and needs to exist (and be verified private) before the
        // first invocation. F2 (security review): this used to `let _ =`
        // the result and spawn tmux regardless — a failed/insecure dir
        // check (F1: a hijacked/symlinked dir, wrong owner, loose mode)
        // silently fell through to placing the socket wherever
        // `secure_private_dir` refused. Now the check must pass, or the
        // spawn is aborted with the reason instead of running open.
        if let Some(dir) = socket.parent() {
            crate::paths::secure_private_dir(dir)
                .with_context(|| format!("securing tmux socket dir {}", dir.display()))?;
        }
        let mut cmd = Command::new("tmux");
        cmd.arg("-S");
        cmd.arg(&socket);
        cmd.args(args);
        let out = cmd
            .output()
            .with_context(|| format!("spawn tmux -S {} {args:?}", socket.display()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "tmux {:?} failed (exit={:?}): {}",
                args,
                out.status.code(),
                stderr.trim()
            );
        }
        Ok(out.stdout)
    }
}

fn parse_session_line(line: &str) -> Result<SessionInfo> {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() != 6 {
        anyhow::bail!("expected 6 fields, got {}: {line:?}", parts.len());
    }
    Ok(SessionInfo {
        name: parts[0].to_string(),
        created: parts[1].parse().with_context(|| format!("created: {:?}", parts[1]))?,
        attached: parts[2] != "0",
        windows: parts[3].parse().with_context(|| format!("windows: {:?}", parts[3]))?,
        // Detached sessions emit empty `session_width`/`session_height` —
        // they only get a size once a client attaches. Treat empty as 0.
        width: parse_u32_or_zero(parts[4], "width")?,
        height: parse_u32_or_zero(parts[5], "height")?,
    })
}

fn parse_u32_or_zero(s: &str, field: &str) -> Result<u32> {
    if s.is_empty() {
        Ok(0)
    } else {
        s.parse()
            .with_context(|| format!("{field}: {s:?}"))
    }
}

fn parse_pane_line(line: &str) -> Result<PaneInfo> {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() != 10 {
        anyhow::bail!("expected 10 fields, got {}: {line:?}", parts.len());
    }
    Ok(PaneInfo {
        id: parts[0].to_string(),
        session: parts[1].to_string(),
        window_index: parts[2].parse().with_context(|| format!("window_index: {:?}", parts[2]))?,
        pane_index: parts[3].parse().with_context(|| format!("pane_index: {:?}", parts[3]))?,
        title: parts[4].to_string(),
        command: parts[5].to_string(),
        pid: parts[6].parse().with_context(|| format!("pid: {:?}", parts[6]))?,
        width: parts[7].parse().with_context(|| format!("width: {:?}", parts[7]))?,
        height: parts[8].parse().with_context(|| format!("height: {:?}", parts[8]))?,
        active: parts[9] != "0",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_session_line_basic() {
        let s = parse_session_line("sot-llm|1778873896|0|1|220|54").unwrap();
        assert_eq!(s.name, "sot-llm");
        assert_eq!(s.created, 1778873896);
        assert!(!s.attached);
        assert_eq!(s.windows, 1);
        assert_eq!(s.width, 220);
        assert_eq!(s.height, 54);
    }

    #[test]
    fn parse_session_line_attached() {
        let s = parse_session_line("session-6|1778786255|1|1|220|54").unwrap();
        assert_eq!(s.name, "session-6");
        assert!(s.attached);
    }

    #[test]
    fn parse_session_line_field_count_mismatch() {
        assert!(parse_session_line("a|b|c").is_err());
    }

    #[test]
    fn parse_session_line_detached_empty_dims() {
        // tmux emits empty session_width/session_height when no client is
        // attached. Real-world output observed on tmux next-3.7.
        let s = parse_session_line("sot-llm|1778873896|0|1||").unwrap();
        assert_eq!(s.width, 0);
        assert_eq!(s.height, 0);
    }

    #[test]
    fn parse_pane_line_basic() {
        let p = parse_pane_line("%23|sot-llm|1|1|bash|julia|1612167|220|54|1").unwrap();
        assert_eq!(p.id, "%23");
        assert_eq!(p.session, "sot-llm");
        assert_eq!(p.window_index, 1);
        assert_eq!(p.pane_index, 1);
        assert_eq!(p.title, "bash");
        assert_eq!(p.command, "julia");
        assert_eq!(p.pid, 1612167);
        assert!(p.active);
    }

    #[test]
    fn parse_pane_line_inactive() {
        let p = parse_pane_line("%5|s|1|2|t|cmd|1234|80|24|0").unwrap();
        assert!(!p.active);
    }

    #[test]
    fn parse_pane_line_rejects_short() {
        assert!(parse_pane_line("a|b|c|d").is_err());
    }

    /// Round-trips a real tmux session through the client. Gated on
    /// `--ignored` since not every test runner has tmux available. Run
    /// manually after wire-protocol changes to catch tmux output drift.
    #[test]
    #[ignore]
    fn integration_round_trip() {
        let c = TmuxClient::new();
        let name = format!("sot-test-{}", std::process::id());
        c.create_session(&name, None, None).expect("create");
        let sessions = c.list_sessions().expect("list-sessions");
        assert!(sessions.iter().any(|s| s.name == name));
        let panes = c.list_panes(Some(&name)).expect("list-panes");
        assert!(!panes.is_empty(), "expected at least one pane in fresh session");
        let captured = c.capture_pane(&panes[0].id, 10).expect("capture-pane");
        // Fresh shell may render empty, but the call has to succeed.
        let _ = captured;
        c.kill_session(&name).expect("kill");
        let after = c.list_sessions().unwrap();
        assert!(!after.iter().any(|s| s.name == name));
    }
}
