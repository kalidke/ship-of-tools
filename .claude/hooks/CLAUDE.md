# .claude/hooks/: the publish guard (records)

This repository is public, so a command that publishes text must not carry a private identifier. `publish-guard.sh` is a
project-scoped Claude Code hook that blocks such a command before it runs. `.claude/settings.json` registers it twice:
as a `PreToolUse` hook on every Bash call, and as `publish-guard.sh --check` at `SessionStart`. Part of records; charter:
docs/CLAUDE.md.

## Files
- `publish-guard.sh`: the hook; blocks `gh pr|issue|release create|edit|comment` and `git commit` text that matches the
  private denylist.
- `publish-guard-test.sh`: black-box cases over a synthetic denylist.
- `README.md`: why the guard sits at this seam, the denylist format, its cost and its known false positives.

## Start here
`README.md` first, for the states (unconfigured, guarded, misconfigured) and the denylist format; then
`publish-guard.sh`, where `resolve_pf` finds the denylist and the loop at the end applies it.

## Rules
- The guard scans only the text after the earliest publish verb, never the whole command line, because a `cd <repo
  path>` prefix legitimately contains denied names; a publish command therefore runs as its own Bash call.
- The denylist is private and never committed: it resolves per call, in order, from `SOT_SCRUB_PATTERNS`,
  `SOT_OPS_DIR/scrub-patterns.txt`, the sibling `ship-of-tools-ops/scrub-patterns.txt`, then
  `.claude/scrub-patterns.local.txt` (`resolve_pf`). A hit is fixed by rewriting the text; only a false positive earns a
  `!span` allow line, and that goes in the private list.
- A clone with no denylist and no marker is silent. A marker (`SOT_SCRUB_PATTERNS` or `SOT_OPS_DIR` set, the sibling
  folder present, or an unreadable local file) without a readable list blocks with exit 2 (`resolve_pf`, `EXPECTED`).
  `--check` reports that state at session start and says nothing otherwise.
- The script stays short and builtin-only (no `jq`, no subprocess): bash reads the whole file on every Bash call, so
  rationale belongs in `README.md`.
- `publish-guard-test.sh` runs in no CI job and no gate script; run it by hand after a change here. Its fixtures are
  synthetic and never name a real identifier.
- Hook registration is read when a session starts; a change to `.claude/settings.json` needs a new session.
