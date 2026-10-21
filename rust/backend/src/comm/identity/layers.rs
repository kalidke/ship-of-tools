//! The agent-layer check (0031, nested agents): a process acts as a handle only if at most one agent lies between it
//! and its row's capsule, or the top of its process tree outside a row. A second agent was started inside the session
//! (`codex exec`, `claude -p`); it, and anything it starts, has no mail identity. Agents are named, not launchers: the
//! list is the agents messaging ships an adapter for.
//!
//! The walk starts at this process and reads each ancestor's whole command line, the caller first, at most
//! [`MAX_RECORDS`]. An ancestry that cannot be read in full before the capsule or the top is refused, never trusted. A
//! process that names a row must reach that row's capsule. Not seen, by design: for a process that names no row, an
//! agent above a reparenting; an agent not in the list; a shim that hides the agent behind another name.
//!
//! Linux reads `/proc`. macOS reads `ps`, which folds runs of white space and prints `(name)` for arguments it cannot
//! read. Windows reads the native snapshot (`registry::ancestors`) and, where an MSYS program's Windows parent has
//! exited (an MSYS program that execs another is a new Windows process), the Git Bash process table its wrapper passes
//! in `SOT_COMM_MSYS`: `<own cygwin pid>;<pid>:<winpid>:<ppid>,...`.

/// The agents messaging ships an adapter for.
const AGENTS: [&str; 2] = ["claude", "codex"];

/// The most records a walk reads.
pub(crate) const MAX_RECORDS: usize = 64;

/// One process of a walk: its arguments, the program first. An argument that could not be read is
/// [`UNKNOWN_ARGS`], which equals nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Rec {
    Proc(Vec<String>),
    /// The walk reached the top of the tree.
    End,
    /// The walk stopped short of the top or the capsule.
    Truncated,
}

/// The one argument of a process whose command line could not be read: arguments unknown.
pub(crate) const UNKNOWN_ARGS: &str = "\u{1e}";

/// How a chain ends for the check.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// Read to the capsule or the top, as the row (or no row) requires.
    Ok,
    /// A process that names a row reached the top, not its capsule.
    NoRow,
    /// A process that names a row reached another row's capsule.
    Elsewhere,
    /// The chain was not read in full.
    Tree,
}

/// The program's name as the check compares it: its file name, a leading `-` (a login shell) and `.exe` dropped,
/// lower case.
fn program_name(w: &str) -> String {
    let w = w.rsplit(['/', '\\']).next().unwrap_or(w);
    let w = w.strip_prefix('-').unwrap_or(w).to_lowercase();
    w.strip_suffix(".exe").map(str::to_string).unwrap_or(w)
}

/// The agent an argument of a `node` process names: a script named after one, or one inside an agent's npm package.
fn agent_of(arg: &str) -> Option<&'static str> {
    let p = arg.replace('\\', "/");
    let base = p.rsplit('/').next().unwrap_or(&p).to_lowercase();
    let base = [".js", ".mjs", ".cjs"].iter().find_map(|x| base.strip_suffix(x)).unwrap_or(&base).to_string();
    if let Some(a) = AGENTS.iter().find(|a| **a == base) {
        return Some(a);
    }
    if p.contains("@anthropic-ai/claude-code") {
        return Some("claude");
    }
    p.contains("@openai/codex").then_some("codex")
}

/// Whether a capsule's arguments before `--` name row `row`'s state: one ends in `/workspaces/<row>` or holds
/// `/workspaces/<row>/voyages/`.
fn capsule_of_row(args: &[String], row: &str) -> bool {
    let k = format!("/workspaces/{row}");
    args.iter().skip(1).take_while(|a| a.as_str() != "--").any(|a| {
        let a = a.replace('\\', "/");
        a.ends_with(&k) || a.contains(&format!("{k}/voyages/"))
    })
}

/// The agent layers of `chain`, nearest first, and how the chain ends; `row` is the row the process names (`""` for
/// none). A `node` host and its native child count once when the child is the host's direct child and its arguments
/// equal the host's after its script; a native agent whose arguments are unknown never counts once with its host.
pub(crate) fn layers(chain: &[Rec], row: &str) -> (Vec<String>, End) {
    let mut out = Vec::new();
    let mut prev: Option<(bool, Option<&'static str>, bool, Vec<String>, bool)> = None;
    for rec in chain {
        let args = match rec {
            Rec::End => return (out, if row.is_empty() { End::Ok } else { End::NoRow }),
            Rec::Truncated => return (out, End::Tree),
            Rec::Proc(a) if a.is_empty() => return (out, End::Tree),
            Rec::Proc(a) => a,
        };
        let w0 = program_name(&args[0]);
        let node = w0 == "node";
        let (name, from) = if node {
            args.iter().enumerate().skip(1).find_map(|(i, a)| agent_of(a).map(|n| (Some(n), i + 1))).unwrap_or((None, 1))
        } else {
            (AGENTS.iter().copied().find(|a| *a == w0), 1)
        };
        let after: Vec<String> = args[from.min(args.len())..].to_vec();
        let unknown = !node && args.get(1).map(String::as_str) == Some(UNKNOWN_ARGS);
        let same_as_child = node
            && prev.as_ref().is_some_and(|(pnode, pname, pagent, pafter, punk)| !pnode && *pagent && !punk && *pname == name && *pafter == after);
        if let (Some(n), false) = (name, same_as_child) {
            out.push(n.to_string());
        }
        prev = Some((node, name, name.is_some(), after, unknown));
        if w0 == "sot-capsule" {
            return (out, if row.is_empty() || capsule_of_row(args, row) { End::Ok } else { End::Elsewhere });
        }
    }
    (out, End::Tree)
}

/// Why this process may not act as a handle: `1` for a second agent inside the session, `2` for an ancestry that
/// cannot be read in full or a process that names a row it is not shown to run inside; the reason is one sentence.
pub(crate) fn judge(chain: Result<Vec<Rec>, String>, row: &str) -> Result<(), (i32, String)> {
    let chain = chain.map_err(|why| (2, unreadable(&why)))?;
    let (agents, end) = layers(&chain, row);
    if end == End::Tree {
        return Err((2, unreadable("")));
    }
    if agents.len() > 1 {
        return Err((
            1,
            format!(
                "this process runs under {}, started inside {}'s session, so it has no comm identity \u{2014} nothing was read, sent or stamped; do not retry or join: an agent that needs its own handle is started as its own row",
                agents[0],
                agents[agents.len() - 1]
            ),
        ));
    }
    if matches!(end, End::NoRow | End::Elsewhere) {
        return Err((
            2,
            format!(
                "this process names row {row} but is not shown to run inside that row's capsule, so it has no comm identity; nothing was read, sent or stamped; start it inside the row, or as its own row"
            ),
        ));
    }
    Ok(())
}

fn unreadable(why: &str) -> String {
    let why = if why.is_empty() { String::new() } else { format!(" ({why})") };
    format!(
        "cannot read this process's ancestry in full (/proc or ps; the process snapshot on Windows), so it cannot be shown to be its session's own agent{why} \u{2014} nothing was read, sent or stamped"
    )
}

/// This process's chain, by the platform's walk.
pub(crate) fn own_chain(env: &crate::comm::Env) -> Result<Vec<Rec>, String> {
    #[cfg(not(windows))]
    let _ = env;
    #[cfg(target_os = "linux")]
    return Ok(linux::chain(std::path::Path::new("/proc"), std::process::id()));
    #[cfg(target_os = "macos")]
    return macos::chain(std::process::id());
    #[cfg(windows)]
    return windows::chain(env.get("SOT_COMM_MSYS").and_then(|v| v.to_str()));
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    return Err("no process walk on this system".into());
}

/// The check for this process, which names row `row` (`""` for none).
pub(crate) fn require_agent(env: &crate::comm::Env, row: &str) -> Result<(), (i32, String)> {
    judge(own_chain(env), row)
}

/// Linux: `/proc/<pid>/stat` for the parent, `/proc/<pid>/cmdline` for the arguments; a newline in an argument is
/// `\x1c`, so an argument that differs from another only by one never reads equal to it.
#[cfg(any(target_os = "linux", test))]
pub(crate) mod linux {
    use super::{Rec, MAX_RECORDS};
    use std::path::Path;

    /// The parent pid in a `stat` line: the field after the command name's closing parenthesis and the state.
    pub(crate) fn ppid_of(stat: &str) -> Option<u32> {
        stat.rsplit_once(") ")?.1.split_whitespace().nth(1)?.parse().ok()
    }

    pub(crate) fn chain(proc: &Path, start: u32) -> Vec<Rec> {
        let mut out = Vec::new();
        let mut p = start;
        loop {
            if out.len() >= MAX_RECORDS {
                out.push(Rec::Truncated);
                return out;
            }
            let Some(pp) = std::fs::read_to_string(proc.join(p.to_string()).join("stat")).ok().as_deref().and_then(ppid_of) else {
                out.push(Rec::Truncated);
                return out;
            };
            let raw = std::fs::read(proc.join(p.to_string()).join("cmdline")).unwrap_or_default();
            let args: Vec<String> = raw
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).replace('\n', "\u{1c}"))
                .collect();
            if args.is_empty() {
                out.push(Rec::Truncated);
                return out;
            }
            out.push(Rec::Proc(args));
            if pp > 1 {
                p = pp;
            } else {
                out.push(Rec::End);
                return out;
            }
        }
    }
}

/// macOS: one `ps -ww -A -o pid= -o ppid= -o args=`, read the way the 0.6.6 scripts read it: every run of white space
/// is a field break, and `(name)` is the name with arguments unknown (`(node)` cannot be told from an agent's host and
/// ends the walk short).
#[cfg(any(target_os = "macos", test))]
pub(crate) mod macos {
    use super::{Rec, MAX_RECORDS, UNKNOWN_ARGS};
    use std::collections::HashMap;

    pub(crate) fn records(table: &str, me: u32) -> Vec<Rec> {
        let mut procs: HashMap<u32, (u32, Rec)> = HashMap::new();
        for line in table.lines() {
            let mut it = line.split_whitespace();
            let (Some(pid), Some(ppid)) = (it.next().and_then(|v| v.parse().ok()), it.next().and_then(|v| v.parse().ok())) else {
                continue;
            };
            let rest: Vec<&str> = it.collect();
            let joined = rest.join(" ");
            let rec = match joined.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
                Some("node") => Rec::Truncated,
                Some(name) => Rec::Proc(vec![name.to_string(), UNKNOWN_ARGS.to_string()]),
                None => Rec::Proc(rest.iter().map(|s| s.to_string()).collect()),
            };
            procs.insert(pid, (ppid, rec));
        }
        let mut out = Vec::new();
        let mut p = me;
        while p > 1 {
            let Some((pp, rec)) = procs.get(&p).filter(|_| out.len() < MAX_RECORDS) else {
                if !out.is_empty() {
                    out.push(Rec::Truncated);
                }
                return out;
            };
            out.push(rec.clone());
            if *rec == Rec::Truncated {
                return out;
            }
            p = *pp;
        }
        if !out.is_empty() {
            out.push(Rec::End);
        }
        out
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn chain(me: u32) -> Result<Vec<Rec>, String> {
        let mut ps = std::process::Command::new("/bin/ps");
        ps.args(["-ww", "-A", "-o", "pid=", "-o", "ppid=", "-o", "args="]);
        let out = crate::lifecycle::child_signal::process()
            .output(&mut ps, std::time::Duration::from_secs(10))
            .map_err(|e| format!("ps: {e}"))?;
        let recs = records(&String::from_utf8_lossy(&out.stdout), me);
        if recs.is_empty() {
            return Err("ps does not list this process".into());
        }
        Ok(recs)
    }
}

/// A Windows command line split the way the Microsoft C runtime splits it: `2n` backslashes and a quote give `n`
/// backslashes and a quote mark, `2n+1` give `n` and a literal quote; inside quotes `""` is one literal quote; an
/// unquoted space or TAB separates.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn split_windows_command_line(cl: &str) -> Vec<String> {
    let c: Vec<char> = cl.chars().collect();
    let (mut out, mut cur, mut quoted, mut has) = (Vec::new(), String::new(), false, false);
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => {
                let mut n = 0;
                while i < c.len() && c[i] == '\\' {
                    n += 1;
                    i += 1;
                }
                has = true;
                if i < c.len() && c[i] == '"' {
                    cur.extend(std::iter::repeat('\\').take(n / 2));
                    if n % 2 == 1 {
                        cur.push('"');
                        i += 1;
                    }
                } else {
                    cur.extend(std::iter::repeat('\\').take(n));
                }
                continue;
            }
            '"' => {
                if quoted && c.get(i + 1) == Some(&'"') {
                    cur.push('"');
                    i += 1;
                } else {
                    quoted = !quoted;
                }
                has = true;
            }
            ' ' | '\t' if !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            ch => {
                cur.push(ch);
                has = true;
            }
        }
        i += 1;
    }
    if has {
        out.push(cur);
    }
    out
}

/// The record for a native process: its exe, then its command line's arguments after argv[0]; a command line that
/// cannot be read is the exe with arguments unknown, and for `node.exe`, which cannot be told from an agent's host, it
/// ends the walk short.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn windows_record(exe: &str, command_line: Option<&str>) -> Rec {
    match command_line.map(split_windows_command_line).filter(|a| !a.is_empty()) {
        None if exe.eq_ignore_ascii_case("node.exe") => Rec::Truncated,
        None => Rec::Proc(vec![exe.to_string(), UNKNOWN_ARGS.to_string()]),
        Some(a) => Rec::Proc(std::iter::once(exe.to_string()).chain(a.into_iter().skip(1)).collect()),
    }
}

/// The Git Bash process table in `SOT_COMM_MSYS`: each Cygwin pid's Windows pid and Cygwin parent (the wrapper's own
/// Cygwin pid leads the text, before the `;`).
#[derive(Debug, Default)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) struct MsysTable {
    /// Cygwin pid -> (Windows pid, Cygwin parent pid).
    pub procs: std::collections::HashMap<u32, (u32, u32)>,
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
impl MsysTable {
    pub(crate) fn parse(text: &str) -> Option<MsysTable> {
        let (own, rest) = text.split_once(';')?;
        own.trim().parse::<u32>().ok()?;
        let procs = rest
            .split(',')
            .filter_map(|e| {
                let mut f = e.split(':').map(|v| v.trim().parse::<u32>().ok());
                Some((f.next()??, (f.next()??, f.next()??)))
            })
            .collect();
        Some(MsysTable { procs })
    }

    /// The Cygwin pid whose current Windows process is `winpid`.
    pub(crate) fn cyg_of(&self, winpid: u32) -> Option<u32> {
        self.procs.iter().find(|(_, (w, _))| *w == winpid).map(|(c, _)| *c)
    }

    /// The Windows pid of `winpid`'s Cygwin parent, when it has one (a Cygwin parent of 1 is a native one).
    pub(crate) fn msys_parent(&self, winpid: u32) -> Option<u32> {
        let (_, pp) = self.procs.get(&self.cyg_of(winpid)?)?;
        (*pp > 1).then(|| self.procs.get(pp).map(|(w, _)| *w)).flatten()
    }
}

/// Windows: the chain from this process up. The parent of a process the MSYS table lists is its Cygwin parent, while
/// it has one; any other parent is the native snapshot's, while it is alive and older than its child. Where the native
/// parent is the top, the walk goes on from the Cygwin parent of the last process, or else of the one before it (the
/// last is then the stub an exec of a native program left).
#[cfg(any(windows, test))]
pub(crate) fn windows_chain(
    start: u32,
    table: &MsysTable,
    native_parent: &dyn Fn(u32) -> Option<u32>,
    record: &dyn Fn(u32) -> Rec,
) -> Vec<Rec> {
    let mut out = vec![record(start)];
    let (mut cur, mut before) = (start, None::<u32>);
    loop {
        if matches!(out.last(), Some(Rec::Truncated)) {
            return out;
        }
        if out.len() >= MAX_RECORDS {
            out.push(Rec::Truncated);
            return out;
        }
        let next = table
            .msys_parent(cur)
            .or_else(|| native_parent(cur))
            .or_else(|| before.and_then(|b| table.msys_parent(b)));
        let Some(next) = next else {
            out.push(Rec::End);
            return out;
        };
        out.push(record(next));
        before = Some(cur);
        cur = next;
    }
}

#[cfg(windows)]
mod windows {
    use super::{windows_chain, windows_record, MsysTable, Rec};

    pub(super) fn chain(msys: Option<&str>) -> Result<Vec<Rec>, String> {
        let snap = crate::comm::registry::ancestors::snapshot().ok_or("no process snapshot")?;
        let table = msys.and_then(MsysTable::parse).unwrap_or_default();
        let created = |pid: u32| crate::comm::registry::ancestors::info(pid).map(|(c, _)| c);
        let native_parent = |pid: u32| {
            let (parent, _) = snap.get(&pid)?;
            let (child_c, parent_c) = (created(pid)?, created(*parent)?);
            (*parent != 0 && *parent != 4 && snap.contains_key(parent) && parent_c <= child_c).then_some(*parent)
        };
        let record = |pid: u32| match snap.get(&pid) {
            Some((_, exe)) => windows_record(exe, crate::comm::registry::ancestors::info(pid).and_then(|(_, cl)| cl).as_deref()),
            None => Rec::Truncated,
        };
        Ok(windows_chain(std::process::id(), &table, &native_parent, &record))
    }
}

#[cfg(test)]
#[path = "layers_tests.rs"]
mod tests;
