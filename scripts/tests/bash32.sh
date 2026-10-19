#!/usr/bin/env bash
# bash32.sh [DIR] -- print the path of a GNU bash 3.2.57 built in DIR (default /scratch/<user>/bash-3.2.57), the bash
# that comm/CLAUDE.md and scripts/CLAUDE.md promise the scripts run under. Built from its tarball (sha256 pinned) once:
# a DIR that already holds a built bash is reused. The build runs in a folder of its own and is moved to DIR whole, so a
# second run racing this one never sees half a build. Needs curl, tar, make, a C compiler, one of bison, yacc or byacc
# (the tarball's parse.y is newer than its y.tab.c, so make runs yacc) and no sudo; a DIR that already holds a built
# bash needs none of them. rust.yml's bash32-linux job and rc-gate.sh both call it. Exit 2 when a need is missing or the
# build fails.
set -u
dir=${1:-/scratch/$(id -un)/bash-3.2.57}
if [ ! -x "$dir/bash" ]; then
    # refuse before the download: a host without a yacc would only fail inside make, after the tarball is fetched
    command -v bison > /dev/null || command -v yacc > /dev/null || command -v byacc > /dev/null || {
        echo "bash32.sh: building bash 3.2.57 needs bison, yacc or byacc (its parse.y is newer than y.tab.c, so make" \
            "runs yacc); install one, or copy a bash built elsewhere to $dir/bash" >&2
        exit 2
    }
    parent=$(dirname "$dir")
    mkdir -p "$parent" || exit 2
    work=$(mktemp -d "$parent/bash32-build-XXXXXX") || exit 2
    trap 'rm -rf "${work:?}"' EXIT
    {
        cd "$work" &&
        curl -fsSL --retry 3 -o bash-3.2.57.tar.gz https://ftp.gnu.org/gnu/bash/bash-3.2.57.tar.gz &&
        echo "3fa9daf85ebf35068f090ce51283ddeeb3c75eb5bc70b1a4a7cb05868bfe06a4  bash-3.2.57.tar.gz" | sha256sum -c - &&
        tar -xzf bash-3.2.57.tar.gz &&
        cd bash-3.2.57 &&
        ./configure --without-bash-malloc --disable-nls \
            CFLAGS='-O1 -std=gnu89 -Wno-implicit-function-declaration -Wno-implicit-int' > configure.log 2>&1 &&
        make > make.log 2>&1
    } > "$work/build.log" 2>&1 || { echo "bash32.sh: the bash 3.2.57 build failed:" >&2; tail -n 40 "$work/build.log" "$work"/bash-3.2.57/configure.log "$work"/bash-3.2.57/make.log >&2 2> /dev/null; exit 2; }
    # a racing build that finished first leaves DIR in place; either result is a built bash
    mv "$work/bash-3.2.57" "$dir" 2> /dev/null || [ -x "$dir/bash" ] || exit 2
fi
echo "$dir/bash"
