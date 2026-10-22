#!/usr/bin/env bash
# gate-tools.sh [DIR] -- print DIR (default /scratch/<user>/gate-tools/bin), a folder holding the three tools the local gate
# needs and a host may lack: jq (a static x86_64 Linux binary of jq 1.7.1), yacc (byacc 20240109 built from its tarball,
# so bash32.sh can build bash 3.2.57) and git (2.49.1 built from its tarball, linked against a zlib 1.3.1 built first,
# with no curl, openssl, gettext, tcl/tk, expat or perl: the local git the tests use; the tests need a git that has
# `init -b`, 2.28 or later). git is installed under DIR/../git, whose libexec holds its helper programs, and DIR holds a
# link to its binary. Each download is checked against a sha256 pinned here before anything
# runs it; a mismatch is refused. git's hash is from https://mirrors.edge.kernel.org/pub/software/scm/git/sha256sums.asc and
# zlib's computed from https://zlib.net/fossils/zlib-1.3.1.tar.gz (zlib.net lists a hash only for its current release). jq's hash is from the release's published sha256sum.txt (jq-linux-amd64,
# https://github.com/jqlang/jq/releases/tag/jq-1.7.1); byacc's was computed from the tarball at
# https://invisible-mirror.net/archives/byacc/, which publishes none. The folder holds only these tools, never a bash,
# sh or other system program, so putting it first on PATH shadows nothing. Idempotent: a tool already there is kept.
# rc-gate.sh puts DIR first on its PATH when it exists (GATE_TOOLS_DIR names another). Needs curl, tar, make and a C
# compiler, and no sudo. Exit 2 when a need is missing, a hash differs or the build fails.
set -u
dir=${1:-/scratch/$(id -un)/gate-tools/bin}
mkdir -p "$dir" || exit 2
work=$(mktemp -d "$dir/../gate-tools-build-XXXXXX") || exit 2
trap 'rm -rf "${work:?}"' EXIT
fetch() { # fetch URL SHA256 FILE
    curl -fsSL --retry 3 -o "$3" "$1" && echo "$2  $3" | sha256sum -c - > /dev/null ||
        { echo "gate-tools.sh: $1 failed or does not match its pinned sha256" >&2; exit 2; }
}
if [ ! -x "$dir/jq" ]; then
    fetch https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-linux-amd64 \
        5942c9b0934e510ee61eb3e30273f1b3fe2590df93933a93d7c58b81d19c8ff5 "$work/jq"
    chmod +x "$work/jq" && mv "$work/jq" "$dir/jq" || exit 2
fi
if [ ! -x "$dir/yacc" ]; then
    (
        cd "$work" || exit 2
        fetch https://invisible-mirror.net/archives/byacc/byacc-20240109.tgz \
            f2897779017189f1a94757705ef6f6e15dc9208ef079eea7f28abec577e08446 byacc.tgz
        tar -xzf byacc.tgz && cd byacc-20240109 && ./configure > configure.log 2>&1 && make > make.log 2>&1 &&
            cp yacc "$dir/yacc.new" && mv "$dir/yacc.new" "$dir/yacc"
    ) || { echo "gate-tools.sh: the byacc build failed" >&2; exit 2; }
fi
if [ ! -x "$dir/git" ]; then
    prefix=$(cd "$dir/.." && pwd)/git
    (
        cd "$work" || exit 2
        fetch https://zlib.net/fossils/zlib-1.3.1.tar.gz \
            9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23 zlib.tgz
        fetch https://mirrors.edge.kernel.org/pub/software/scm/git/git-2.49.1.tar.xz \
            310831de967f1c8c5e8ff55f92807dea89f83dc3d3d2a5d16c209bd01a31def1 git.tar.xz
        tar -xzf zlib.tgz && (cd zlib-1.3.1 && ./configure --prefix="$prefix/zlib" --static > configure.log 2>&1 &&
            make > make.log 2>&1 && make install > install.log 2>&1) &&
            tar -xJf git.tar.xz && cd git-2.49.1 &&
            make -j"$(nproc)" prefix="$prefix" ZLIB_PATH="$prefix/zlib" NO_CURL=1 NO_OPENSSL=1 NO_GETTEXT=1 NO_TCLTK=1 \
                NO_EXPAT=1 NO_PERL=1 NO_PYTHON=1 install > make.log 2>&1 &&
            ln -sfn "$prefix/bin/git" "$dir/git"
    ) || { echo "gate-tools.sh: the git build failed" >&2; exit 2; }
fi
echo "$dir"
