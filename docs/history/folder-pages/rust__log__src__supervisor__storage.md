# rust/log/src/supervisor/storage: the storage wait (capsule)

When the state root's volume is full, the authority neither counts the failure against its legs nor goes terminal:
it holds in one wait, probes the root with a real durable write on a backoff, and resumes when a probe succeeds.
Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: `leg_death`, `Wait` and `Resume`, `advance`, `account`, `probe` (the durable state-root write) and `delay`.
- `tests.rs`: the wait's backoff, resumes and refusals with an injected probe and clock, and the probe on a real directory.

## Start here
`advance` for one tick of the wait; `leg_death` for which leg exits enter it.

## Rules
- Storage exhaustion is `host::storage_exhaustion`'s answer, never an error's text.
- The wait never charges or resets the crash counter and never enters Terminal for storage; a probe error that is not storage exhaustion, a probe still running after 60 s, or a probe worker gone without a result ends it Terminal.
- One probe runs at a time, after 1, 2, 4, 8 and 16 s and then every 30 s; the step carries across a storage exit that follows a successful probe and returns to 0 when a leg is judged stable (`account`).
- The probe writes and syncs a new file (`.storage-probe-<nonce>`) and syncs the state root: that is success. It then removes the file; a file it cannot remove is a leftover the wait removes later by its absolute path, never by listing the folder, and a name it did not create is never removed.
- A resumed leg runs the same voyage with the full argv; a resumed authority operation re-runs startup recovery, which finishes an interrupted end_run or reset from its journal.
