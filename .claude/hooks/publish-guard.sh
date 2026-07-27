#!/usr/bin/env bash
# publish-guard.sh — project-scoped Claude Code PreToolUse hook (matcher: Bash).
# Blocks a Bash call that would publish personal/operational identifiers to a
# public surface: gh pr/issue/release create-edit-comment, and git commit.
# Exit 0 = allow (silent), exit 2 = block (stderr shown to the agent).
#
# Rationale, pattern-file format, costs and limitations: see README.md here.
# Tests: ./publish-guard-test.sh
# Kept deliberately short — bash lexes the whole file on every Bash tool call,
# so prose in this script is paid for on all of them. It belongs in the README.
set -u

IN="${CLAUDE_TOOL_INPUT:-}"
if [ -z "$IN" ] && [ ! -t 0 ]; then IFS= read -r -d '' IN 2>/dev/null || true; fi
[ -z "$IN" ] && exit 0

case "$IN" in
    *'gh pr create'*|*'gh pr edit'*|*'gh pr comment'*|\
    *'gh issue create'*|*'gh issue edit'*|*'gh issue comment'*|\
    *'gh release create'*|*'gh release edit'*|\
    *'git commit'*) ;;
    *) exit 0 ;;
esac

# Scan only what the verb PUBLISHES, not the whole command line. Commands are
# routinely prefixed with `cd <repo path>`, and that path legitimately contains
# identifiers the denylist blocks — scanning it made every commit unpublishable.
# Longest suffix == earliest verb, so a later `&& cd ...` is still covered.
target=""
for v in 'gh pr create' 'gh pr edit' 'gh pr comment' \
         'gh issue create' 'gh issue edit' 'gh issue comment' \
         'gh release create' 'gh release edit' 'git commit'; do
    case "$IN" in
        *"$v"*) s="${IN#*"$v"}"; [ ${#s} -gt ${#target} ] && target="$s" ;;
    esac
done
# Empty tail = nothing published inline (e.g. bare `git commit` opens an editor).
[ -z "$target" ] && exit 0

# Denylist resolution, most specific first. No readable file => silent no-op, so
# a public cloner gets a working repo rather than a broken or noisy hook.
PF=""
# Expand to locals FIRST: `${CLAUDE_PROJECT_DIR%/*}` on an unset var is fatal
# under `set -u`, which would exit 1 on every publish command outside a project.
proj="${CLAUDE_PROJECT_DIR:-}"
ops="${SOT_OPS_DIR:-}"
for cand in \
    "${SOT_SCRUB_PATTERNS:-}" \
    "${ops:+$ops/scrub-patterns.txt}" \
    "${proj:+${proj%/*}/ship-of-tools-ops/scrub-patterns.txt}" \
    "${proj:+$proj/.claude/scrub-patterns.local.txt}"
do
    [ -n "$cand" ] && [ -r "$cand" ] && { PF="$cand"; break; }
done
[ -z "$PF" ] && exit 0

text="${target,,}"
deny=()
while IFS= read -r line || [ -n "$line" ]; do
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    [ -z "$line" ] && continue
    case "$line" in
        '#'*) ;;
        # Allow spans are erased BEFORE denying, so a legitimate context that
        # contains a denied span (a public URL carrying an owner name) survives.
        '!'*) a="${line:1}"; a="${a,,}"; [ -n "$a" ] && text="${text//"$a"/}" ;;
        *)    deny+=("${line,,}") ;;
    esac
done < "$PF"

for d in ${deny+"${deny[@]}"}; do
    [ -z "$d" ] && continue
    case "$text" in
        *"$d"*)
            cat >&2 <<EOF
BLOCKED by .claude/hooks/publish-guard.sh — this repo is PUBLIC.

The command would publish text containing a private identifier: "$d"

Rewrite with the neutral vocabulary this repo already uses — "the maintainer",
"a peer session", "the backend host", "a shared host" — then run it again.
Technical content (dates, measurements, ports, rationale) is never the problem;
who and where is.

False positive? Add a "!<span>" allow line to the denylist, which lives in the
private ops sidecar — never in this repo. See .claude/hooks/README.md.
EOF
            exit 2
            ;;
    esac
done
exit 0
