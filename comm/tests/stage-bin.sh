#!/usr/bin/env bash
# stage-bin.sh DEST -- lay out the comm scripts flat in DEST in the repo's form:
# every regular file directly in each folder of comm/bin-folders.txt (CLAUDE.md
# excepted), one folder after another, so comm-lib.sh and sot-fe source their parts
# beside them. The installer puts each part inside the file that sources it instead
# (src/comm_bin.jl); test/install_tests.jl shows comm-lib.sh defines the same
# functions and globals in both. Users: the suites' guard (lib-home-guard.sh's
# guard_stage_bin), comm-matrix.sh's live run, scripts/docs-media.sh and the
# sot-setup no-Julia recipe. It sources nothing, so none of them needs the guard.
# bash 3.2 safe.
set -u
[ $# -eq 1 ] && [ -n "$1" ] || { echo "usage: stage-bin.sh DEST" >&2; exit 2; }
DEST="$1"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" || exit 2
LIST="$ROOT/comm/bin-folders.txt"
[ -f "$LIST" ] || { echo "stage-bin: FATAL no $LIST"; exit 1; }
mkdir -p "$DEST" || { echo "stage-bin: FATAL cannot make $DEST"; exit 1; }

# publish_file SRC DEST MODE -- DEST becomes a copy of SRC by a rename from an exclusive sibling, so a reader of DEST
# sees all its old bytes or all its new ones. The copy takes no source permissions (-p fails across filesystems that
# cannot hold them); the mode is set before the rename. On any failure only this call's own temporary is removed
# and DEST is left as it was.
publish_file() {
    local src="$1" dest="$2" mode="$3" tmp
    tmp="$(mktemp "${dest%/*}/.stage.${dest##*/}.XXXXXX")" || return 1
    if cp "$src" "$tmp" && chmod "$mode" "$tmp" && mv -f "$tmp" "$dest"; then return 0; fi
    rm -f "${tmp:?}"
    return 1
}

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
        # Mode: a script, or any executable source, is 0755; the rest 0600.
        case "$name" in *.sh) mode=0755 ;; *) if [ -x "$f" ]; then mode=0755; else mode=0600; fi ;; esac
        publish_file "$f" "$DEST/$name" "$mode" || { echo "stage-bin: FATAL cannot publish $f as $DEST/$name"; exit 1; }
        n=$((n + 1))
    done
    [ "$n" -gt 0 ] || { echo "stage-bin: FATAL $line ships no file"; exit 1; }
done < "$LIST"
