//! `sotd comm <verb>`: messaging's commands, which every session runs through the shell wrappers it types
//! (`comm-send.sh` and its siblings) and the shell door the other scripts source (`comm/lib/comm-lib-sotd.sh`). Each
//! verb keeps its script's arguments, output words and exit codes. It runs on any box that shares a home with a
//! daemon host, with no daemon of its own: the writes are this binary's own code, under the same locks the daemon
//! takes.

mod book;
mod door;
mod guard;
mod join;
mod poll;
mod send;
mod start;
mod wire;

pub(crate) const USAGE: &str = "usage: sotd comm <verb> [args]
  send @h 'msg' | @h --file <path|-> | @h - | --broadcast 'msg'
                          file one message (comm-send.sh)
  relay send @h 'msg'     the same, the way comm-relay.sh takes it
  poll                    show the mail past the read cursor, then move it
  context [--rebind]      this process's identity, eval-able (comm-context.sh)
  join [--name N] [--repin] [--expertise E]
                          take this session's handle (comm-join.sh)
  leave [--name H]        give it up, or remove an orphan's entry
  list                    the registered handles
  session-start           identity, join and the start lines
  unread <h>              `<total> <unread>` for the end-of-turn check
  stop-mark <h>           the Stop hook's mark for the wake
  subagent-guard          the PreToolUse hook's decision (input on stdin)
  require-agent | routable <h> <root> | registry <read|put|del|touch|del-provisional> ... | derive ... |
  self-slot <host> <ws> | row-id | name-part ... | handoff-line <h> | ensure-home
                          the shell door's functions (comm-lib-sotd.sh)";

/// What a verb printed and its exit code: the verbs return it, so their words are tested without a process.
#[derive(Debug, Default)]
pub(crate) struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub(crate) fn out(&mut self, line: impl AsRef<str>) {
        self.stdout.push_str(line.as_ref());
        self.stdout.push('\n');
    }

    pub(crate) fn err(&mut self, line: impl AsRef<str>) {
        self.stderr.push_str(line.as_ref());
        self.stderr.push('\n');
    }

    pub(crate) fn errs(&mut self, lines: &[String]) {
        lines.iter().for_each(|l| self.err(l));
    }

    pub(crate) fn exit(mut self, code: i32) -> Out {
        self.code = code;
        self
    }
}

/// Runs `sotd comm <args>`; the process's exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    let Some((verb, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let out = match verb.as_str() {
        "send" => send::send(rest),
        "relay" => send::relay(rest),
        "poll" => poll::poll(rest),
        "context" => book::context(rest),
        "join" => join::join(rest),
        "leave" => book::leave(rest),
        "list" => book::list(rest),
        "session-start" => start::session_start(rest),
        "unread" => poll::unread(rest),
        "stop-mark" => door::stop_mark(rest),
        "subagent-guard" => guard::run(rest),
        "require-agent" => door::require_agent(rest),
        "routable" => door::routable(rest),
        "registry" => door::registry(rest),
        "derive" => door::derive(rest),
        "self-slot" => door::self_slot(rest),
        "slug" => door::slug(rest),
        "handoff-line" => door::handoff_line(rest),
        "ensure-home" => door::ensure_home(rest),
        other => {
            let mut o = Out::default();
            o.err(format!("sotd comm: unknown verb {other:?}\n{USAGE}"));
            o.exit(2)
        }
    };
    use std::io::Write;
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    out.code
}

/// The agent-layer check for this process (`identity::layers`), for the row its identity names.
pub(crate) fn gate() -> Result<(), (i32, String)> {
    let host = sot_log::host::state_dir::host_name().unwrap_or_default();
    crate::comm::identity::layers::require_agent(&crate::comm::identity::context::row_id(&host))
}

/// This process's identity, after the gate passed: the comm folder is made whole when it is not.
pub(crate) fn context(rebind: bool, notes: &mut Vec<String>) -> Result<crate::comm::identity::context::Context, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("comm-context: no working folder ({e})"))?;
    crate::comm::identity::context::resolve(rebind, &cwd, || true, notes)
}

/// The identity's own refusal for sending as it (`sotd comm routable`): a handle, an entry for it, and that entry's
/// root this project's (or none, an entry older than roots). `bin` is where the session's scripts are, for the
/// recovery line.
pub(crate) fn routable(name: &str, project_root: &str, registry: &std::path::Path) -> Result<(), String> {
    use crate::comm::registry::registry::{entry, Entry};
    let bin = bin_dir();
    let q = |s: &str| crate::comm::identity::context::shell_quote(s);
    if name.is_empty() {
        return Err("your sot-comm identity did not resolve \u{2014} refusing to send with no verifiable from-handle (a reply would silently misroute). Join first: comm-join.sh (if you held a handle before, see the sot-session-start skill's recovery recipe).".into());
    }
    match entry(registry, name) {
        Entry::Absent => Err(format!("your identity @{name} has no registry row; join again: {}/comm-join.sh", q(&bin))),
        Entry::Unreadable => Err(format!("the registry could not be read, so identity @{name} is unverified; nothing was sent")),
        Entry::Present(e) => {
            let root = e.get("root").and_then(serde_json::Value::as_str).unwrap_or("");
            if !root.is_empty() && !crate::comm::identity::paths::same_root(root, project_root) {
                return Err(format!(
                    "your sot-comm identity '@{name}' is registered to a DIFFERENT project's root ('{root}') \u{2014} refusing to send with a misrouting from-handle. Join again: {}/comm-join.sh",
                    q(&bin)
                ));
            }
            Ok(())
        }
    }
}

/// The folder of the session's comm scripts: `SOT_COMM_BIN`, which every wrapper sets, else `<comm home>/bin`.
pub(crate) fn bin_dir() -> String {
    std::env::var("SOT_COMM_BIN").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| {
        crate::comm::home::comm_home().map(|h| crate::comm::identity::paths::native(&h.join("bin").to_string_lossy())).unwrap_or_default()
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
