Part of rows: see ../../../rust/backend/src/rows/CLAUDE.md (work-state); the Stop hook's mail gate and comm-subagent-guard.sh are messaging's (../../CLAUDE.md).

`comm-status-idle.sh` checks unread mail before markers. A blocked closing marker carries this convention on one
line, followed by the context needed to answer cold:

    SITREP-QUESTION: <question> Irreversible: <what happens>. Pane only: <what>. Options: (a) ...; (b) .... Recommend (a). Page: <absolute path>

`Irreversible:`, `Pane only:` and `Page:` are optional; `Options:` with at least two `(letter)` choices before a
`Recommend` with a letter is required. Read rows' grammar (`work_state/question.rs`): remove the first `Page:` value,
split at the first `Options:`, then end options at the first valid `Recommend[ed][:] [(]letter[)]`. An incomplete
question blocks once before stamping or flooring, keeping any earlier blocked declaration. A stop-hook continuation
never gets that shape nudge and stamps as usual. Any error while checking passes the stop. A parked blocked turn
without a marker is owed this same whole line and then its context. Language rules stay in the sitrep skill.
