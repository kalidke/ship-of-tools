# rust/backend/src/sidecars/child: what each child the daemon starts carries with it (sidecars)

A Julia child gets a private temporary folder, made before its spawn and removed after its reap, so what a killed child
leaves goes with it; the daemon's controlled end removes the folders of the children it still holds, and a start those of
a daemon that ended without one. A Julia or MathJax child's stderr is drained to its end and its last lines kept, so the
owner of a child that failed to start logs the child's own words at warn. Part of the sidecars; charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `mod.rs`: declares the modules and re-exports `ChildTmp`, `remove_own`, `sweep_gone` and `StderrTail`.
- `stderr_tail.rs`: a child's stderr drained to its end, its last lines kept for the warn its owner logs (`StderrTail`).
- `tmp.rs`: `ChildTmp` (`new`, `for_user_code`, `apply`, `display_ended`, `retire`), the end's and the start's removals
  (`remove_own`, `sweep_gone`), with the tests of its folder, its name, its environment, its end and the removals' rule.

## Start here
`ChildTmp::retire`.

## Rules
- The folder is named in the child's environment (`TMPDIR`; `TMP` and `TEMP` on Windows). `retire` ends the child, then
  the child's `display/` sibling (a REPL's or Pluto's own X display), then removes the folder.
- `display_ended` returns once that display has ended under the child, and never for a child with none; an owner waits
  on it beside the child's own exit for the child's whole life and then retires the child.
- A `ChildTmp` dropped after a spawn may still hold a live child's files; only an owner whose child never started drops
  one.
- The folder is `sot-julia-<pid>-<random>` in the daemon's temporary folder, `<pid>` the daemon's own, so its name says
  whose it is. A daemon's controlled end runs no owner's `retire`, so its terminal (`lifecycle::shutdown::exit` and
  `exit_by_signal`), after the fire, removes every folder that names its pid (`remove_own`), waiting at most
  `FOLDERS_WAIT` (2 s). An end that runs no code in the daemon (a crash, a SIGKILL of the daemon or of its Linux guard)
  removes nothing, so each start (`server::run`) removes, on a blocking thread nothing waits for, the folders of every
  pid that is not running (`sweep_gone`). Both remove only a directory this account owns, never a link or a file, and
  log a folder that does not go.
- A pid counts as running when this account may not signal or open it, and an unreaped one (a zombie) does too, so the
  folders of a running daemon, this one included, always stay. A pid the OS has reused keeps a dead daemon's folder
  until the process that now has it ends; the controlled end of any `sotd` with that pid removes it as its own.
- Limits: a folder named `sot-julia-<random>`, a daemon's from before the pid was in the name, names no owner and is
  never removed. A pid is read on this computer and in this process's pid namespace, so a temporary folder that `sotd`
  processes on two computers or in two pid namespaces share is outside the rule. A folder a child still writes into
  while the terminal removes it (on Windows, one in which a child still ending holds a file open), or that is not
  removed within `FOLDERS_WAIT`, waits for the next start.
