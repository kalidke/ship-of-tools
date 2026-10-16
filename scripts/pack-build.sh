#!/usr/bin/env bash
# pack-build.sh -- pack this checkout's Linux build the way release.yml's Linux job does, for `install.sh --archive`.
#
#   scripts/pack-build.sh <out-dir>
#
# Builds the frontend (glibc) and sotd + sot-capsule (musl static) from this checkout, and writes into <out-dir>
# sot-<version>-linux-x86_64.tar.gz, COMMIT (this checkout's HEAD) and SHA256SUMS (the tarball and COMMIT). <version>
# is the built sotd's own, so a build of the line packs as `0.6.6-rc9.14-dev+<sha>`. Refuses a dirty tree, a HEAD no
# remote-tracking branch holds (the install checks the commit out of origin) and a host with no musl C compiler (a
# glibc sotd stops mail on the oldest fleet hosts). Linux only. Build output goes in ${CARGO_TARGET_DIR:-rust/target}.

set -euo pipefail

cd "$(dirname "$0")/.."

[ $# -eq 1 ] || { echo "usage: scripts/pack-build.sh <out-dir>" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "pack-build: Linux x86_64 only (this is $(uname -sm))" >&2; exit 1; }

command -v x86_64-linux-musl-gcc >/dev/null || command -v musl-gcc >/dev/null \
    || { echo "pack-build: no musl C compiler (x86_64-linux-musl-gcc or musl-gcc) on PATH: install musl-tools" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "pack-build: the working tree is not clean; commit or stash first" >&2; exit 1; }
[ -n "$(git branch -r --contains HEAD)" ] \
    || { echo "pack-build: HEAD is on no remote-tracking branch; push the line first" >&2; exit 1; }

mkdir -p "$1"
out="$(cd "$1" && pwd)"
target="${CARGO_TARGET_DIR:-$PWD/rust/target}"
commit="$(git rev-parse HEAD)"

# The same commands as release.yml's "Build release binaries" and "Build sotd + sot-capsule (musl static, linux)".
(
    cd rust
    export CARGO_TARGET_DIR="$target"
    cargo build --release -p sot-frontend --locked
    cargo build --release -p sot-backend -p sot-log --bin sotd --bin sot-capsule --target x86_64-unknown-linux-musl --locked
)

musl="$target/x86_64-unknown-linux-musl/release"
v="$("$musl/sotd" --version | cut -d' ' -f2)"
stage="sot-$v-linux-x86_64"
work="$(mktemp -d "${TMPDIR:-/tmp}/sot-pack.XXXXXX")"
trap 'rm -rf "${work:?}"' EXIT
mkdir "$work/$stage"
cp "$target/release/sot" "$musl/sotd" "$musl/sot-capsule" "$work/$stage/"
got="$("$work/$stage/sot" --version)"
case "$got" in "sot $v ("*) ;; *) echo "pack-build: VERSION MISMATCH for sot: $got (sotd says $v)" >&2; exit 1 ;; esac
cp deploy/sotd.service "$work/$stage/"
cp scripts/sot-apply.sh "$work/$stage/sot-apply"
tar -czf "$out/$stage.tar.gz" -C "$work" "$stage"
printf '%s\n' "$commit" > "$out/COMMIT"
(cd "$out" && sha256sum "$stage.tar.gz" COMMIT > SHA256SUMS)
echo "packed $v at $out"
