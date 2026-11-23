Part of rows: see ../../rust/backend/src/rows/CLAUDE.md (work-state).

The Codex row hooks pass the root event's nonempty conversation and turn ids through the environment to the
existing writes. A root prompt records the pair with its floor, clearing an earlier owner if either id is missing.
A permission, tool return or Stop whose UUIDv7 timestamp predates the owner leaves the registry untouched. The Stop mark checks before mail, with the same lock wait. An unreadable entry or a
missed lock lets the Stop continue. An older binary ignores the pair and the hooks keep their usual marker writes.
The final matching Stop drops the pair. The matching Interrupt also drops the floor, dialog and stop mark without
setting done. A child event follows the ordinary hook path. See rows' contract for incomplete input and switches.
