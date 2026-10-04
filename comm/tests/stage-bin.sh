#!/usr/bin/env bash
# stage-bin.sh DEST -- lay out the comm scripts flat in DEST the way the installer
# lays out ~/.sot-comm/bin: every regular file directly in each folder of
# comm/bin-folders.txt (CLAUDE.md excepted), one folder after another. Users: the
# suites' guard (lib-home-guard.sh's guard_stage_bin), comm-matrix.sh's live run,
# scripts/docs-media.sh and the sot-setup no-Julia recipe. It sources nothing, so
# none of them needs the guard. bash 3.2 safe.
set -u
[ $# -eq 1 ] && [ -n "$1" ] || { echo "usage: stage-bin.sh DEST" >&2; exit 2; }
DEST="$1"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" || exit 2
LIST="$ROOT/comm/bin-folders.txt"
[ -f "$LIST" ] || { echo "stage-bin: FATAL no $LIST"; exit 1; }
mkdir -p "$DEST" || { echo "stage-bin: FATAL cannot make $DEST"; exit 1; }

seen=$'\n'
while IFS= read -r line || [ -n "$line" ]; do
    line="${line//[ $'\r']/}"
    [ -n "$line" ] || continue
    dir="$ROOT/$line"
    [ -d "$dir" ] || { echo "stage-bin: FATAL $line is not a folder: $dir"; exit 1; }
    n=0
    for f in "$dir"/* "$dir"/.[!.]*; do
        [ -f "$f" ] || continue
        name="${f##*/}"
        [ "$name" != CLAUDE.md ] || continue
        case "$seen" in
            *$'\n'"$name"$'\n'*) echo "stage-bin: FATAL $name is shipped by two folders (again by $line)"; exit 1 ;;
        esac
        seen="$seen$name"$'\n'
        cp -pf "$f" "$DEST/$name" || { echo "stage-bin: FATAL cannot copy $f"; exit 1; }
        case "$name" in *.sh) chmod 0755 "$DEST/$name" || exit 1 ;; esac
        n=$((n + 1))
    done
    [ "$n" -gt 0 ] || { echo "stage-bin: FATAL $line ships no file"; exit 1; }
done < "$LIST"
