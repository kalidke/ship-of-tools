# rust/backend/src/sidecars/child: what each child the daemon starts carries with it (sidecars)

A Julia child gets a private temporary folder, made before its spawn and removed after its reap, so what a killed child
leaves goes with it. Part of the sidecars; charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `mod.rs`: declares the modules and re-exports `ChildTmp`.
- `tmp.rs`: `ChildTmp` (`new`, `for_user_code`, `apply`, `retire`), with the tests of its folder, its environment and
  its end.

## Start here
`ChildTmp::retire`.

## Rules
- The folder is named in the child's environment (`TMPDIR`; `TMP` and `TEMP` on Windows). `retire` ends the child, then
  the child's `display/` sibling (a REPL's or Pluto's own X display), then removes the folder.
- A `ChildTmp` dropped after a spawn may still hold a live child's files; only an owner whose child never started drops
  one.
