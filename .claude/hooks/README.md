# Project hooks

## `publish-guard.sh`

A project-scoped Claude Code `PreToolUse` hook (matcher: `Bash`) that blocks a
command which would publish personal or operational identifiers to a public
surface — `gh pr|issue|release create/edit/comment`, and `git commit`.

### Why this seam

This repo is public. A 2026-07-25 audit of all three public surfaces found
real-world identifiers — a maintainer's name, machine hostnames, internal
workspace names, an absolute home path — in commit messages and PR bodies, all
of them drafted from pasted agent output.

A git `pre-commit` hook cannot catch the worst of that: `gh pr create --body`
never touches a commit, so PR and issue text is invisible to git entirely. This
hook sits where that text is actually born — the Bash call itself — which is the
only place both surfaces are visible at once.

CI is the wrong layer for a different reason: by the time a workflow runs, the
branch is already pushed and public. CI can gate a merge; it cannot prevent a
leak.

### Scope

Deliberately **project-level** (`.claude/settings.json` in this repo), not a
globally installed hook. Every other repo on the machine is unaffected.

### The denylist is private and lives outside this repo

The pattern file names the very identifiers that must not appear here, so
committing it would leak exactly what the guard protects. Resolution order, most
specific first:

1. `$SOT_SCRUB_PATTERNS` — explicit override (used by the tests)
2. `$SOT_OPS_DIR/scrub-patterns.txt` — the private ops sidecar; **prefer this**,
   it syncs across machines
3. `<repo-parent>/ship-of-tools-ops/scrub-patterns.txt`
4. `.claude/scrub-patterns.local.txt` — gitignored, per-machine fallback

**No readable pattern file means the hook is a silent no-op.** A public cloner
gets a working repo, never a broken or noisy hook.

### Pattern file format

One entry per line; `#` comments and blank lines ignored.

| Line | Meaning |
|---|---|
| `somestring` | **deny** — block if it appears (case-insensitive substring) |
| `!somestring` | **allow** — erase these spans *before* denying |

Allow entries are what make a coarse denylist usable. They are applied first, so
a legitimate context that contains a denied span survives — e.g. `!github.com/<owner>`
rescues the repo's own badge and clone URLs while the bare owner name stays
deniable.

Keep every allow span as narrow as possible: a broad one silently punches a hole
through every deny entry it contains. Prefer a more specific *deny* over a broad
*allow* — the current denylist needs no allow entries at all, because it denies
`/home/<user>` rather than the bare owner name that appears in ~25 legitimate
URLs. **A guard that cries wolf is a guard that gets switched off.**

### Cost

Measured on the Linux backend, interleaved A/B, min of 5 rounds × 150 calls
(±2 ms run-to-run noise on a loaded box, so treat these as approximate):

| | ms/call |
|---|---|
| bare `bash` spawn + `exit 0` (the floor) | 2.07 |
| **`publish-guard.sh`, non-publish call (~99% of calls)** | **4.21** |
| `repo-boundary-guard.sh` (already installed, same event) | 12.49 |

Matchers filter on *tool name*, not command content, so this runs on every Bash
call and decides for itself to bail. The non-publish path costs one bash spawn, a
stdin read, and one `case` — the pattern file is not even opened.

Two things follow, and they drive the implementation:

- **No subprocesses.** The same logic written with `jq` measured 23 ms — 5× the
  whole hook — because `jq` is a second spawn. Everything here is a bash builtin.
- **Keep the script short.** bash lexes the whole file on every call; stripping
  comments from an earlier 7.6 KB draft cut ~2 ms. That is why the rationale
  lives in this README and the tests live in a separate file.

On a Windows git-bash box a single process spawn costs **0.4–1.4 s** (measured
2026-07-26 from a frontend session). If this guard is ever installed there, that
spawn — not the script — is the entire budget. Installing it only on the machine
where commits and PRs are actually created is the sane default.

### What gets scanned

Only the text **after** the publish verb — not the whole command line.

This is not a refinement, it is load-bearing. Commands are routinely prefixed
with `cd <repo path>`, and that path legitimately contains identifiers the
denylist blocks, so scanning the full line made every commit unpublishable. The
guard blocked its own commit exactly this way while being built. A path inside
the *message* is still caught, which is the case that matters.

Longest-suffix wins, so the earliest verb anchors the scan and a later
`&& cd ...` is still covered.

### Known false-positive class

The gate matches publish verbs anywhere in the command string, including inside a
quoted payload. So a command *about* publishing — benchmarking this hook, grepping
history for `git commit`, or writing docs that quote a `gh pr create` example —
is treated as a real publish and its tail is scanned.

Fail-closed by design. Workaround: put the text in a file and have the command
read it, rather than inlining it on the command line — which is how this hook's
own benchmark and end-to-end fixtures are written.

### Tests

```bash
.claude/hooks/publish-guard-test.sh
```

13 black-box cases over a synthetic pattern file (the fixtures cannot use the
real denylist for the same reason the denylist is not committed). Covers
allow/deny/case-insensitivity, allow-span precedence, each publish surface, and
the unconfigured no-op path — that last one caught a `set -u` unbound-variable
bug that made the guard exit 1 on every publish command outside a project dir.
