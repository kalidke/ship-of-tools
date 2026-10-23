#!/usr/bin/env bash
# legacy-skill-hashes.sh -- list every version of a Claude skill this repo ever shipped, one `<name> <hash>` per line.
#
# An install that moves the skills out of a person's own skills folder (src/skills.jl, `_retire_placed_skills!`) removes
# a folder only when its contents hash to one of these lines. The hash is the sha256 of the folder's manifest: one line
# `<path under the skill>\t<git blob sha1>` per file, sorted, `CLAUDE.md` files left out (an install never copies them).
# The versions are frozen history, so the list is committed (comm/legacy-skills.txt) and this script is rerun only to
# audit it. Usage: scripts/legacy-skill-hashes.sh > comm/legacy-skills.txt   (from a full clone; reads git history)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
names="julia-repl project-log reauth show-result sitrep sot-comm sot-gh-auth sot-install sot-session-start sot-setup sot-status sot-statusline-setup worktree sot-be-session-start sot-fe-session-start"
for name in $names; do
    for base in agents/claude comm/adapters/claude .claude/skills; do
        dir="$base/$name"
        for commit in $(git log --format=%H -- "$dir") ; do
            manifest="$(git ls-tree -r --full-tree "$commit" -- "$dir" | awk -F'\t' -v d="$dir/" '
                { split($1, m, " "); path = substr($2, length(d) + 1); if (path ~ /(^|\/)CLAUDE\.md$/) next; print path "\t" m[3] }' | LC_ALL=C sort)"
            [ -n "$manifest" ] || continue
            printf '%s %s\n' "$name" "$(printf '%s' "$manifest" | sha256sum | cut -d' ' -f1)"
        done
    done
done | LC_ALL=C sort -u
