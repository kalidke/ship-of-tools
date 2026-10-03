# Why Ship of Tools exists, and the principles every change keeps

Read this first, before any design page or code. It is short on purpose. Where today's code breaks a principle, the
line says so; that is a known gap to close, never a pattern to copy.

## Why it exists

Ship of Tools is an agentic development environment for Julia. AI agents drive the interface, the REPL and
navigation to read, run and surface code; the developer steers, watches and reviews. It keeps ordinary editor and REPL
mechanics and adds a concept explorer on top. Agents work in sessions (rows) that the developer runs on several
machines, and the sessions message each other so the agents can coordinate. `requirements.md` says what the system
does; `CLAUDE.md` says how it is built.

Three processes, even on one machine: a frontend (Rust, GPU-rendered window, stateless about the project), a backend
daemon (Rust, owns project state, rows and the processes it starts), and a Julia kernel (plugin host and project
introspector). They talk over a socket, so a remote machine is the same protocol on a different transport.

## Principles

1. **Simple and elegant, but no simpler.** Every design and review round asks two questions: what is missing, and what
   can be deleted. A field, type or setting names the invariant it serves, or it is a candidate for deletion. Stripping
   past an invariant (durability, identity, an honest record) is not simplicity.
2. **Fix the cause, and prefer the design that removes a failure class over one that handles it.** When a measurement
   shows a mechanism is unreliable, re-plan; do not add rules to it.
3. **One owner for each piece of state and each process lifecycle.** The daemon that starts a row owns that row and the
   processes in it (ADR 0046, ADR 0050). One writer per file. Today several programs append to an inbox under a shared
   lock; that is a known gap.
4. **Identity is issued, not inferred.** A session's handle is declared to the daemon that owns its row (ADR 0046).
   Today the comm scripts still check identity by walking the process tree; that is a known gap, and no change should
   lean on it further.
5. **Messaging is one page: ADR 0049.** A message is delivered when it is appended to the receiver's inbox file. A send
   has one result, `filed` or `FAILED`; nothing is queued and there is no second route. The daemon is the only wake: it
   types one fixed line into a row with unread mail at a free prompt, and never into a dialog, menu, draft or working
   session.
6. **Delete the old path in the same change that adds the new one.** No side-by-side mechanisms.
7. **Logic lives in the compiled core.** Rust is boring plumbing (rendering, IPC, process supervision); Julia holds
   plugin and Julia-aware logic, and plugins extend the system through multiple dispatch with no privileged access.
   Shell and PowerShell are thin entry points. The boundaries between processes are serialization seams.
8. **Premises are tested before code, on every platform the change ships to:** Linux, native Windows, git-bash (MSYS),
   and NFSv3 and NFSv4 home folders. A premise about an outside product (Claude Code, an OS, a library) is re-tested when
   that product updates. What has been learned the hard way is in `docs/platform-facts.md`.
9. **Tests come from the requirement and fail first on the defect.** They run isolated (their own HOME and folders),
   never against the live system, and their fixtures for outside interfaces are real captures.
10. **Reactive over eager; features wait until forced; defects do not.** A version freezes its features, not its fixes:
    every known bug in the current feature set is fixed in that version.
11. **The repository is public.** No host names, user names, addresses or session handles in code, commits or records.
