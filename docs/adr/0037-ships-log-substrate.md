# ADR 0037: The Ship's Log — sessions become durable records

**Status:** Accepted (2026-08-23). Built in phases: the first piece (ADR 0038) ships
immediately and needs none of the new machinery; the rest lands step by step as a new
crate in the existing Rust workspace.
**Date:** 2026-08-23

> How this was designed: an extended back-and-forth between the maintainer's Claude
> session and a second AI model acting as critic, with the requirements deliberately
> kept open. Two earlier designs were discarded for concrete flaws — a single central
> session daemon (restarting it would kill every session, which is the very problem
> being solved) and an "immortal" per-session helper (nothing that holds a live
> terminal can promise to never die, and code that is never upgraded keeps its bugs
> forever). The third design below survived the criticism.

## The problem, in plain words

Today a session *is* a running program. Your conversation with Claude lives inside
that process; the Julia REPL's loaded packages and variables live inside that
process. When the process dies — a daemon restart, a reboot, a crash — the session is
gone, and there is no record of what it did. Everything we have built to cope (tmux,
keep-alive tunnels, the frontend relaunch dance) is scaffolding to keep fragile
processes alive, and it still fails: one daemon restart in August killed every
session on the machine at once.

## The idea

Flip it: **the session is a notebook, not a program.** Every session gets a
permanent, append-only record — everything typed in, everything printed out, every
turn, every figure produced — written down as it happens. The running program is just
the thing *currently writing in the notebook*. If it crashes, nothing of the session
is lost: the notebook is already on disk. Start a new writer, hand it the notebook,
continue.

Once that is taken seriously, several hard features stop being features and simply
follow from what a notebook is:

- **Surviving restarts** — notebooks don't die when programs die.
- **Scrollback and replay** — flip to an earlier page.
- **Pausing a session for a week** — a notebook nobody is writing in. Costs nothing.
- **Moving a session to another machine** — carry the notebook over, start a writer
  there.
- **Forking** — two continuations that share their first forty pages, like a git
  branch.
- **A permanent, searchable archive** of everything your agents ever did — with
  every figure traceable to the exact moment and session that produced it.

## The four parts

- **Voyage** — the notebook itself: one session's permanent record. (A ship's log is
  exactly this — the durable record of a journey — hence the name.) Technically: a
  stable ID plus an append-only chain of sealed, checksummed record files.
- **Capsule** — one writer: a small process that babysits one running program
  (Claude, a Julia REPL, a simulation, an instrument acquisition) and writes its
  voyage. One capsule per running session, so if a capsule dies, exactly *one*
  session hiccups — never the whole fleet. That was the fatal flaw of tmux and of the
  first draft of this design: one big process holding everyone's sessions.
- **Catalog** — the card index in the library, one per machine: it finds notebooks
  and starts writers, but holds no truth of its own. Delete it and it rebuilds itself
  from the notebooks. Restarting or upgrading it is a non-event.
- **Bridge** — how another machine reads or writes a notebook: plain SSH, which the
  fleet already trusts. No new open ports, no passwords, no certificates to manage.

Terminals are just the *first kind* of writer. A headless Claude run driven through
its SDK, a Julia REPL speaking our existing typed frames, a long simulation, or a
microscope acquisition are all equally sessions — same notebook format, same tools.

## The rules that keep it honest

1. Any helper (daemon, catalog, viewer) can restart or upgrade **without killing
   anyone's session**.
2. When something that owns a live process fails, it takes down **at most that one
   session's current run** — never its record, never its neighbors.
3. Whatever the record says happened **stays on disk through any crash**.
4. Upgrades of the writer itself either hand over gracefully or honestly restart
   that one session. A security fix may end a run; it never ends a voyage.

## Details that matter (the fine print)

- **What "saved" means.** Input is written to disk *before* the program sees it. A
  crash can only cut off the unfinished tail of the record, never the middle. A
  figure is saved before anything else is allowed to point at it.
- **Who may type.** One writer at a time. Watching is free for everyone; typing
  requires explicitly *taking* the pen, and taking it cleanly revokes the previous
  holder — so a stalled laptop's buffered keystrokes can never land in the middle of
  a command you're typing from another machine. Reconnecting makes you a watcher,
  never silently the typist again.
- **What an acknowledgment means.** "Your input was recorded", never "the agent has
  acted on it". If the connection drops mid-send, the sender can safely ask and
  resend without double-typing.
- **Passwords stay out.** Typed input is *not* stored by default (the record keeps
  that input happened, not the bytes) — otherwise a password typed at a hidden
  prompt would become permanent plaintext history. Storing input is per-session
  opt-in.
- **Disk full stops the session visibly** instead of silently dropping history.
- **Old records are packed, never rewritten.** Compaction may re-bundle files, but
  the content is immutable; "cleanup" is never a silent delete.
- **Identity is written down from day one.** Every record carries which voyage,
  which run, and what came before it — because you cannot retrofit "where did this
  come from" into an archive later. The fancy machinery on top (branching UI,
  cross-fleet timelines, clever clocks) deliberately waits.
- **Two classes of program.** Agents with real control interfaces (Claude via its
  SDK, Codex via its server protocol, our Julia REPL shim) give the record true
  turns, tool calls, and artifacts. Everything else gets a faithful terminal
  recording. We never *guess* turn boundaries from raw terminal bytes — that guess
  cannot be made reliable.

## How machines connect

A **star, not a mesh**: each frontend machine dials each backend machine directly,
over SSH, using the host list that already exists in `hosts.toml`. Backends never
talk to each other, and there is no distributed bookkeeping — four SSH connections,
nothing else. Sessions on a laptop are **private automatically**: the laptop accepts
no incoming connections, so nobody can even ask. Publishing work is a deliberate
act — move the notebook to a backend ("promotion") and it becomes visible to every
frontend.

Security is what we *removed*: no listening ports, no tokens, no certificate
authority, nothing to rotate. SSH between machines; ordinary owner-only file
permissions on disk and on local sockets; records live on each machine's own disk
(never on the shared network home, which cannot host them safely).

## What happens to old records

Each session declares its fate when it starts, with sensible defaults per kind:

- **archive** — pack the full record off to bulk storage. For work whose history
  matters.
- **discard** — throw the record away when the session ends. For scratch work and
  sensitive work; costs nothing while running.
- **distill** — have an LLM read the finished record and keep only a written
  summary. The bytes age out; the meaning survives, and the summary stays linked to
  where it came from. This is machine-generated handoff — the form future sessions
  actually consume.

Defaults, the summary prompt, and pack layout are still open decisions.

## Build order

- **P0 (ships now, ADR 0038):** stop daemon restarts from killing tmux sessions.
  Pure systemd; no new code concepts.
- **P1:** the capsule and the record format, on Linux, recording terminals. Gated on
  crash-injection tests — the file format is read forever, so it gets the care
  first.
- **P2:** the first rich writer: Claude through its SDK.
- **P3 / P4 (either order):** capsules on the frontend machines (macOS, then
  Windows) so local sessions survive frontend restarts — this retires the ADR 0017
  relaunch dance; and remote attach over SSH — this retires "quit and relaunch to
  switch hosts" (ADR 0015).
- **P5:** the daemon's terminal plumbing moves onto capsules behind a switch; the
  tmux path is deleted only after a long soak with a tested way back. The machine
  monitor (ADR 0020) becomes just another writer, which finally makes its history
  survive restarts.
- **Later, cheaply, because the records make them cheap:** forking, moving sessions
  between machines, fleet-wide timelines, session-to-session messaging over the same
  records, and other viewers (chat, voice, mobile).

## What this costs (stated honestly)

- The record format and its crash-safety are real engineering that renders no
  pixels. The phasing forces each step to pay rent.
- tmux and capsules coexist for a while; the session list must show which is which.
- The archive is valuable and therefore sensitive: it needs a backup plan and gets
  treated like a vault. Discard-mode and the no-stored-input default exist for
  exactly this.
- More small processes instead of one big tmux server; the catalog must make the
  fleet easy to inspect or it becomes opaque in a new way.

## Names

Session = **voyage**; one run of it = a **leg**; the whole system = **the Ship's
Log**. The nautical word and the technical word mean the same thing here, which is
why the name is right. Binary names are fixed as each piece lands (`sot-bridge` for
remote attach).
