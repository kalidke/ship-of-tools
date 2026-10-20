# rust/backend/src/sidecars/child: what each child the daemon starts carries with it (sidecars)

A Julia child, and each command the updater runs, gets a private temporary folder, made before its spawn and removed
after its reap, so what a killed or failed child leaves (Pkg's package download among it) goes with it; the daemon's
controlled end removes the folders of the children it still holds, and a start those of a daemon that ended without
one. A Julia or MathJax child's stderr is drained to its end and its last lines kept, so the
owner of a child that failed to start logs the child's own words at warn, and a caller still waiting on a child that has
not said it is ready is told how long it has been starting and what it last wrote (`Start`). Part of the sidecars;
charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `mod.rs`: declares the modules and re-exports `ChildTmp`, `remove_own`, `sweep_gone`, `StderrTail` and `Start`.
- `stderr_tail.rs`: a child's stderr drained to its end, its last lines kept for the warn its owner logs (`StderrTail`)
  and read while it starts, for the one phrase a caller waiting on its start is told (`Start::still_starting`).
- `tmp.rs`: `ChildTmp` (`new`, `for_user_code`, `apply`, `display_ended`, `retire`), the process's held folder of them
  (`InHeld`, its count) and its removals at the end and the start (`remove_own`, `sweep_gone`), with the tests of its
  folder, its held folder and its end.

## Start here
`ChildTmp::retire`.

## Rules
- The folder is named in the child's environment (`TMPDIR`; `TMP` and `TEMP` on Windows). `retire` ends the child, then
  the child's `display/` sibling (a REPL's or Pluto's own X display), then removes the folder.
- `display_ended` returns once that display has ended under the child, and never for a child with none; an owner waits
  on it beside the child's own exit for the child's whole life and then retires the child.
- A `ChildTmp` dropped after a spawn may still hold a live child's files; only an owner whose child never started drops
  one, or one that drops the child's `Contained` first, which asks its tree to end: the updater's spawner
  (`rust/backend/src/update.rs` `UpdaterSpawner`) makes the folder before the child, so a call dropped past its bound
  ends the command's tree before the folder goes.
- The folder is `sotd-julia-<random>/child-<random>` in the daemon's temporary folder: every child folder of a process
  is in that process's own folder of them, a held folder (platform's `HeldDir`) made with its first child folder,
  removed with its last (`InHeld`) and held by a lock until then. A daemon's controlled end runs no owner's `retire`,
  so its terminal (`lifecycle::shutdown::exit` and `exit_by_signal`), after the fire, removes that folder
  (`remove_own`), waiting at most `FOLDERS_WAIT` (2 s); a process that holds no child folder removes nothing. An end
  that runs no code in the daemon (a crash, a SIGKILL of the daemon or of its Linux guard) removes nothing, so each
  start (`server::run`) removes, on a blocking thread nothing waits for, the held folder of every process whose lock is
  free (`sweep_gone`, platform's `remove_ended`): only a directory this account owns, never a link or a file, and a
  folder that does not go is logged.
- Liveness is the lock, never a pid: a running process's folder stays, this one's included, whatever computer or pid
  namespace it runs in and whatever pid it has, and an ended one's goes though another process now has its pid.
- Limits: a folder named `sot-julia-*`, a daemon's from before held folders, is never removed. A temporary folder that
  `sotd` processes on two computers share is inside the rule only on a file system whose locks every client sees (NFS
  with its lock service; not one mounted `nolock` or with `local_lock`), and then only while the maker's client holds
  its lease: a computer cut off from the NFS server longer than its lease (90 s by default) loses its lock, and
  another computer's start can then remove its live folder. A temporary folder on a file system that refuses locks
  (NFSv3 without its lock service) stops every kernel, REPL and Pluto start and every updater command, as `ChildTmp::new` fails. A folder a child still writes into while the terminal removes it (on Windows, one in which a
  child still ending holds a file open), or that is not removed within `FOLDERS_WAIT`, waits for the next start, and so does the held folder of a process that ended without the terminal
  while it held child folders, or kept one whose child was not confirmed ended. A program that does not take its
  temporary folder from the environment makes its files outside the child's folder, and they stay: macOS's `mktemp`
  given no template uses the account's own (`getconf DARWIN_USER_TEMP_DIR`). Julia's `tempdir()`, and so Pkg's
  downloads, read `TMPDIR`.
