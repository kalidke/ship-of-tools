#!/usr/bin/env bash
# publish-guard.sh — project-scoped Claude Code PreToolUse hook (matcher: Bash).
# Blocks a Bash call that would publish personal/operational identifiers to a
# public surface: gh pr/issue/release create-edit-comment, and git commit.
# Exit 0 = allow (silent), exit 2 = block (stderr shown to the agent).
#
# `--check` reports readiness as a SessionStart systemMessage instead of guarding.
#
# Rationale, pattern-file format, costs and limitations: see README.md here.
# Tests: ./publish-guard-test.sh
# Kept deliberately short — bash lexes the whole file on every Bash tool call,
# so prose in this script is paid for on all of them. It belongs in the README.
set -u

# Sets PF (readable denylist, or empty) and EXPECTED (1 = this machine is
# supposed to be guarded). Resolution runs per invocation, NOT at session start,
# so a box that acquires the file mid-session is protected immediately.
resolve_pf() {
    PF=""
    # Expand to locals FIRST: `${CLAUDE_PROJECT_DIR%/*}` on an unset var is fatal
    # under `set -u`, which would exit 1 on every publish command outside a project.
    proj="${CLAUDE_PROJECT_DIR:-}"
    ops="${SOT_OPS_DIR:-}"
    override="${SOT_SCRUB_PATTERNS:-}"
    # Normalise separators before any `%/*`: that cannot strip a BACKSLASH, so a
    # native Windows path would resolve the sidecar candidate INSIDE the project
    # dir rather than beside it — a silent miss on a correct layout.
    proj="${proj//\\//}"; ops="${ops//\\//}"; override="${override//\\//}"

    sidecar="${proj:+${proj%/*}/ship-of-tools-ops}"
    localpf="${proj:+$proj/.claude/scrub-patterns.local.txt}"

    for cand in \
        "$override" \
        "${ops:+$ops/scrub-patterns.txt}" \
        "${sidecar:+$sidecar/scrub-patterns.txt}" \
        "$localpf"
    do
        [ -n "$cand" ] && [ -r "$cand" ] && { PF="$cand"; break; }
    done

    # A public cloner has no denylist and must not get a broken or noisy hook, so
    # unconfigured stays a silent allow. But anything asserting "protection belongs
    # here" — an explicit override/ops path, or the private sidecar merely EXISTING
    # — makes a missing denylist a hard failure instead of a quiet one.
    EXPECTED=0
    [ -n "$override" ] && EXPECTED=1
    [ -n "$ops" ] && EXPECTED=1
    [ -n "$sidecar" ] && [ -d "$sidecar" ] && EXPECTED=1
    [ -n "$localpf" ] && [ -e "$localpf" ] && EXPECTED=1
    return 0
}

# SessionStart readiness. Silent when guarded or when genuinely unconfigured;
# speaks only in the state that used to be invisible. Deliberately NOT a
# per-invocation warning — a notice on every commit is trained out within a day.
if [ "${1:-}" = "--check" ]; then
    resolve_pf
    if [ -z "$PF" ] && [ "$EXPECTED" -eq 1 ]; then
        printf '%s\n' '{"systemMessage":"publish-guard is INERT: a denylist is expected on this machine but none was readable, so publishing to public surfaces is BLOCKED until it resolves. Usual cause is a stale ops sidecar (fetch + pull it) or a layout mismatch (export SOT_OPS_DIR). See .claude/hooks/README.md."}'
    fi
    exit 0
fi

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
# Checked BEFORE resolution on purpose: this path exits 0 even on a fully guarded
# box, so failing it closed would add friction to a case the guard never covered.
[ -z "$target" ] && exit 0

resolve_pf
if [ -z "$PF" ]; then
    [ "$EXPECTED" -eq 0 ] && exit 0
    cat >&2 <<'EOF'
BLOCKED by .claude/hooks/publish-guard.sh — this repo is PUBLIC and the guard is INERT.

A denylist is expected on this machine (an ops sidecar exists, or SOT_OPS_DIR /
SOT_SCRUB_PATTERNS is set) but none was readable, so NOTHING was scanned. This
blocks rather than passing silently: an unguarded box used to be indistinguishable
from a protected one, and was only ever caught by someone going to look.

Fix — any one of:
  * fetch + pull the ops sidecar        (usual cause: a stale checkout; "0 behind"
                                         without a fetch is measured against a stale ref)
  * export SOT_OPS_DIR=<abs path>       (sidecar isn't beside the project, or the
                                         hook env hands over a native Windows path)
  * export SOT_SCRUB_PATTERNS=<file>    (point straight at a denylist)

The denylist resolves per invocation, so a fix applies immediately — no restart.
See .claude/hooks/README.md.
EOF
    exit 2
fi

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
