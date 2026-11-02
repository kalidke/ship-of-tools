#!/usr/bin/env bash
# test-bash32.sh: bash32.sh builds bash 3.2.57 with make, and the tarball's parse.y is newer than its y.tab.c, so make
# runs yacc. A host with none of bison, yacc or byacc must stop before the download, and a DIR that already holds a built
# bash is reused with no yacc at all. A failed GNU download tries the mirror, and a corrupt mirror response fails its
# checksum before extraction. Nothing is downloaded: curl is a recording stub. Exit: failed checks.
set -u
script=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bash32.sh
# macOS's TMPDIR ends in a slash: strip it, or the paths below hold `//`.
base="${TMPDIR:-/tmp}"
work=$(mktemp -d "${base%/}/bash32-test-XXXXXX") || exit 2
trap 'rm -rf "${work:?}"' EXIT
sh_bash=$(command -v bash)
nl=$'\n'
fail=0
check() { if eval "$2"; then echo "PASS: $1"; else echo "FAIL: $1 (rc=$rc: $out)"; fail=$((fail + 1)); fi; }
run() { out=$(env PATH="$1" "$sh_bash" "$script" "$2" 2>&1); rc=$?; }
stub() { printf '#!/bin/sh\nexit 0\n' > "$1" && chmod +x "$1"; }
# a folder of links to only the tools bash32.sh needs besides a yacc, and a curl stub that records its call and fails
bin_for() {
    mkdir -p "$work/$1/bin"
    for t in dirname mkdir mktemp mv rm sha256sum tail tar make; do ln -s "$(command -v "$t")" "$work/$1/bin/$t"; done
    printf '#!/bin/sh\n: > "%s"\nexit 1\n' "$work/$1/curl.called" > "$work/$1/bin/curl"
    chmod +x "$work/$1/bin/curl"
    echo "$work/$1/bin"
}

# case 1: no yacc on PATH, so the script refuses before it downloads anything
b=$(bin_for none)
run "$b" "$work/none/bash-3.2.57"
check "no yacc: the exit is 2" '[ "$rc" = 2 ]'
check "no yacc: the line names bison and yacc" '[[ $out == *bison* && $out == *yacc* ]]'
check "no yacc: the message is one line" '[[ $out != *"$nl"* ]]'
check "no yacc: curl is never called" '[ ! -e "$work/none/curl.called" ]'

# case 2: a DIR that already holds a built bash is reused, yacc or not
b=$(bin_for reuse)
mkdir -p "$work/reuse/bash-3.2.57"
stub "$work/reuse/bash-3.2.57/bash"
run "$b" "$work/reuse/bash-3.2.57"
check "a built bash in DIR is reused with no yacc: its path is printed, exit 0" '[ "$rc" = 0 ] && [ "$out" = "$work/reuse/bash-3.2.57/bash" ]'
check "a built bash in DIR is reused: curl is never called" '[ ! -e "$work/reuse/curl.called" ]'

# case 3: each of the three yacc names lets the build go on to its download
for y in bison yacc byacc; do
    b=$(bin_for "$y")
    stub "$b/$y"
    run "$b" "$work/$y/bash-3.2.57"
    check "a $y on PATH: the build goes on to its download, which fails here" '[ -e "$work/$y/curl.called" ] && [[ $out == *"build failed"* ]]'
done

# case 4: GNU is unavailable, the mirror returns corrupt bytes, and the real checksum check refuses extraction
b=$(bin_for mirror)
stub "$b/yacc"
cat > "$b/curl" <<'CURL'
#!/bin/sh
for url do :; done
printf '%s\n' "$url" >> "$BASH32_DOWNLOADS"
case "$url" in
    https://ftp.gnu.org/*) exit 1 ;;
    https://mirrors.kernel.org/*) printf 'corrupt archive\n' > bash-3.2.57.tar.gz ;;
    *) exit 2 ;;
esac
CURL
mv "$b/tar" "$b/tar-real"
cat > "$b/tar" <<'TAR'
#!/bin/sh
: > "$BASH32_EXTRACTED"
exit 1
TAR
chmod +x "$b/curl" "$b/tar"
export BASH32_DOWNLOADS=$work/downloads BASH32_EXTRACTED=$work/extracted
run "$b" "$work/mirror/bash-3.2.57"
check "download failure tries GNU then the fallback mirror" '[ "$(cat "$BASH32_DOWNLOADS")" = "https://ftp.gnu.org/gnu/bash/bash-3.2.57.tar.gz${nl}https://mirrors.kernel.org/gnu/bash/bash-3.2.57.tar.gz" ]'
check "a corrupt mirror download fails the checksum before extraction" '[ "$rc" = 2 ] && [[ $out == *"checksum did NOT match"* ]] && [ ! -e "$BASH32_EXTRACTED" ]'
echo "failed: $fail"
exit "$fail"
