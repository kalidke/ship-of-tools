//! `sotd comm subagent-guard`: the Claude Code `PreToolUse` hook on Bash that keeps a subagent off mail (0031: a
//! subagent uses no mail; it reports to its parent, which sends). Nothing in a subagent's shell environment or ancestry
//! differs from its session's, so the commands cannot tell it apart; the hook input can: Claude Code adds `agent_id`
//! to the input of a hook inside a subagent, and the session's own input has none. With `agent_id` set, a Bash command
//! that runs an installed comm script is denied. Every other case is allowed silently (no `agent_id`, any other
//! command, input that does not parse): a session is never locked out of its own mail.

use crate::comm::Env;
use std::io::Read;

use super::Out;

const DENY: &str = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"A subagent does not use sot-comm; report to your parent session, which sends."}}"#;

/// The length of a comm script's name at the start of `s` (`comm-<word>.sh`), when there is one.
fn script_at(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("comm-")?;
    let word = rest.bytes().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')).count();
    (word > 0 && rest[word..].starts_with(".sh")).then_some(5 + word + 3)
}

/// The length of one word that may come before a command (`bash`, `sh`, `exec`, `command`, `env`, `NAME=value`, an
/// option), when `s` starts with one.
fn lead_at(s: &str) -> Option<usize> {
    let word = s.find(char::is_whitespace).unwrap_or(s.len());
    let w = &s[..word];
    let assignment = w.split_once('=').is_some_and(|(n, _)| {
        let mut c = n.chars();
        c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
    });
    let option = w.len() > 1 && w.starts_with('-') && w[1..].bytes().all(|b| b.is_ascii_alphabetic());
    (matches!(w, "bash" | "sh" | "exec" | "command" | "env") || assignment || option).then_some(word)
}

/// Whether `cmd` runs an installed comm script: a path into `.sot-comm/bin`, or a bare `comm-*.sh` with no folder in
/// command position (what `PATH` resolves to the installed bin), optionally after the words [`lead_at`] knows. Quotes
/// and backslashes inside a word do not hide it. A comm script that is an argument (`grep`, `cat`, `git`) or lies under
/// any other folder (a checkout's `comm/mail/`) is allowed. Read line by line, as `grep` read it.
pub(crate) fn runs_a_comm_script(cmd: &str) -> bool {
    let cmd: String = cmd.chars().filter(|c| !matches!(c, '\'' | '"' | '\\')).collect();
    cmd.lines().any(|line| {
        if let Some(i) = line.find(".sot-comm/bin/") {
            if script_at(&line[i + ".sot-comm/bin/".len()..]).is_some() {
                return true;
            }
        }
        let starts = std::iter::once(0).chain(line.char_indices().filter(|(_, c)| ";&|(`".contains(*c)).map(|(i, c)| i + c.len_utf8()));
        starts.into_iter().any(|start| {
            let mut s = &line[start..];
            loop {
                s = s.trim_start();
                if let Some(n) = script_at(s) {
                    let next = s[n..].chars().next();
                    if next.map_or(true, |c| c.is_whitespace() || ";&|)`".contains(c)) {
                        return true;
                    }
                }
                match lead_at(s) {
                    Some(n) if s[n..].starts_with(char::is_whitespace) => s = &s[n..],
                    _ => return false,
                }
            }
        })
    })
}

/// The decision for one hook input.
pub(crate) fn decide(input: &str) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let agent = v.get("agent_id").map(|a| a.as_str().map_or_else(|| a.to_string(), str::to_string)).unwrap_or_default();
    if agent.is_empty() || agent == "null" {
        return None;
    }
    let cmd = v.get("tool_input").and_then(|t| t.get("command")).and_then(serde_json::Value::as_str).unwrap_or("");
    runs_a_comm_script(cmd).then_some(DENY)
}

pub(crate) fn run(_env: &Env, _args: &[String]) -> Out {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let mut o = Out::default();
    if let Some(d) = decide(&input) {
        o.out(d);
    }
    o
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;
