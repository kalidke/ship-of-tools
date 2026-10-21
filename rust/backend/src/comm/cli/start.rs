//! `sotd comm session-start` (comm-session-start.sh): the receive bootstrap. Resolve the identity, join, print one
//! `BOOTSTRAP-ARM` line and the start lines, and stop: the daemon wakes the row, so there is nothing to arm.

use super::{context, gate, join, Out};

/// The work-state rule, printed on every start: the row's color is read from it.
const WORKSTATE_RULE: &str = "Work-state (nav row colour) \u{2014} stamp it yourself with comm-status.sh <working|waiting|blocked|done|idle> \"why\":
  Priority: the white result badge above all, then red, then GREEN, then purple, then blue/gray.
  Red and green outrank purple STRUCTURALLY. While your turn is running the row is GREEN, even if a job you launched is also running \u{2014} purple is a BETWEEN-turns colour, for when nothing of yours is running but something you launched is still outstanding. Red is a question that needs the user once your turn has ENDED; a question raised mid-turn shows green until the turn stops, which is correct: you are still working.
  Stamp waiting (purple) the moment you delegate; it is sticky across turns until you stamp working/idle/done when the job lands.
  A background job does NOT make you idle. If ANY item needs the user while jobs also run, the turn ends blocked (SITREP-QUESTION, the question first, jobs listed after); waiting only when nothing needs the user. Full mechanics: the sot-comm skill's references/work-state.md.
Turn end: when a turn CLOSES an effort (a result landed, a fix shipped, a diagnosis reached) or ends parked, its last block opens with a marker line \u{2014} SITREP: <headline> (done) / SITREP-QUESTION: <question> (blocked) / SITREP-WAITING: <what for> (waiting) \u{2014} followed by the sitrep chain in plain words (the sitrep skill: no hashes, paths, names, backticks or bullets). The Stop hook stamps the row from that line. A step in a live back-and-forth owes NO block: answer and end.";

/// What a session cannot discover on its own, printed beside the rule.
const CAPABILITIES: &str = "Julia: this workspace has a PERSISTENT REPL you can drive \u{2014} sot-fe repl eval \"$SOT_WORKSPACE_ID\" --code '<code>' | sot-fe repl run <ws> <file.jl>. Packages stay loaded between runs and the call returns real output; use it instead of spawning julia for anything that re-pays a heavy package load. One eval at a time, and Main is shared with the owner's drawer \u{2014} read the julia-repl skill before the first call.";

/// The session's recovery file, `<project root>/dev/output/handoff-<handle>.md`, as one line when it exists.
pub(crate) fn handoff_line(handle: &str, cwd: &std::path::Path) -> Option<String> {
    if handle.is_empty() {
        return None;
    }
    let root = crate::comm::identity::paths::raw_root(cwd);
    let file = format!("{root}/dev/output/handoff-{handle}.md");
    std::path::Path::new(&file)
        .is_file()
        .then(|| format!("HANDOFF: read {file} before other work; it is this session's recovery file."))
}

pub(crate) fn session_start(args: &[String]) -> Out {
    let mut o = Out::default();
    if !args.is_empty() {
        o.err("usage: comm-session-start.sh");
        return o.exit(2);
    }
    let fail = |mut o: Out| {
        o.out("BOOTSTRAP-ARM handle=none identity=FAIL WAKE: n/a");
        o
    };
    if let Err((_, why)) = gate() {
        o.err(format!("comm-session-start: {why}"));
        return fail(o);
    }
    let mut notes = Vec::new();
    let ctx = match context(false, &mut notes) {
        Ok(c) => c,
        Err(e) => {
            o.errs(&notes);
            o.err(e);
            return fail(o);
        }
    };
    o.errs(&notes);
    // A daemon-pinned self file that does not exist yet: this row never joined, so it has no identity to keep.
    let cold = std::env::var_os("SOT_COMM_SELF_FILE").is_some_and(|f| !f.is_empty() && !std::path::Path::new(&f).is_file());
    if !ctx.name_held_by.is_empty() && !ctx.row_id.is_empty() {
        let held = crate::comm::identity::self_file::read(&ctx.self_file).map(|f| f.name).unwrap_or_default();
        o.out(format!("comm-session-start: @{held} moved to row {}; this row ({}) joins it again", ctx.name_held_by, ctx.row_id));
    }
    // The slot already names another project's identity (the context discarded it): refuse.
    if !cold && ctx.name.is_empty() && ctx.name_held_by.is_empty() && ctx.self_file.is_file() {
        o.out("BOOTSTRAP-ARM handle=none identity=FAIL WAKE: n/a");
        o.err(format!(
            "REFUSED: {} already names a different, validated identity (see the diagnostic line above) \u{2014} refusing to join over it. A row's self file is its own; a session outside any row that shares a slot is started with a private SOT_COMM_SELF_FILE of its own; nothing started inside a session joins. See this skill's Identity line and references/reclaim-handle.md.",
            ctx.self_file.display()
        ));
        return o;
    }
    let joined = join::join(&[]);
    let mut text = joined.stderr.clone();
    text.push_str(&joined.stdout);
    o.stdout.push_str(&text);
    let handle = joined
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("Joined sot-comm as @"))
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("")
        .to_string();
    if handle.is_empty() {
        return fail(o);
    }
    let identity = if text.contains("still being heartbeated") { "MISMATCH" } else { "ok" };
    o.out(format!("BOOTSTRAP-ARM handle={handle} identity={identity} WAKE: daemon"));
    if let Some(line) = handoff_line(&handle, &std::env::current_dir().unwrap_or_default()) {
        o.out(line);
    }
    o.out(WORKSTATE_RULE);
    o.out(CAPABILITIES);
    o
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
