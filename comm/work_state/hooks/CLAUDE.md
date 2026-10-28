Part of rows: see ../../../rust/backend/src/rows/CLAUDE.md (work-state); the Stop hook's mail gate and comm-subagent-guard.sh are messaging's (../../CLAUDE.md).

`comm-status-idle.sh` checks unread mail before markers. A blocked closing marker carries this convention on one
line, followed by the context needed to answer cold:

    SITREP-QUESTION: <question> Irreversible: <what happens>. Options: (a) ...; (b) .... Recommend (a). Page: <absolute path>

`Irreversible:` is optional (an older line's `Pane only:` passes the check and stamps as before); `Options:` with at least two `(letter)` choices before a
`Recommend` with a letter is required. Read rows' grammar (`work_state/question.rs`): remove the first `Page:` value,
split at the first `Options:`, then end options at the first valid `Recommend[ed][:] [(]letter[)]`. An incomplete
question blocks once before stamping or flooring, keeping any earlier blocked declaration. A stop-hook continuation
never gets that shape nudge and stamps as usual. A question that owes a decision page (rows' `question::page_owed`:
an `Irreversible:` or `Pane only:` clause, or more than two sentences before `Options:`, with no `Page:`) is refused by
the stamp; on a turn's first stop the hook turns that refusal into one nudge to make the page and keeps the row, and a
continuation stamps the line as written (`COMM_STATUS_PAGE_NUDGED=1`, which `comm-status.sh` passes as
`--page-nudged`). Any error while checking passes the stop. A parked blocked turn
without a marker is owed this same whole line and then its context. Language rules stay in the sitrep skill.
