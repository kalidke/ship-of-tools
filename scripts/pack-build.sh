#!/usr/bin/env bash
# pack-build.sh -- pack this checkout's Linux build the way release.yml's Linux job does, for `install.sh --archive`.
#
#   scripts/pack-build.sh <out-dir>
#
# Builds the frontend (glibc) and sotd + sot-capsule from this checkout, and writes into <out-dir>
# sot-<version>-linux-x86_64.tar.gz, COMMIT (this checkout's HEAD), LIBC (the libc the daemon binaries target) and
# SHA256SUMS (the tarball, COMMIT and LIBC). <version> is the built sotd's own, so a build of the line packs as
# `0.6.6-rc9.14-dev+<sha>`. sotd and sot-capsule are musl static when a musl C compiler is on PATH, as release.yml's
# Linux job builds them. With none (musl-tools needs sudo) they are built for the host's gnu target, which runs on every
# fleet host only when this host's glibc is no newer than FLEET_GLIBC_FLOOR: a newer host refuses. Also refuses a
# dirty tree and a HEAD no remote-tracking branch holds (the install checks the commit out of origin). Linux only.
# Build output goes in ${CARGO_TARGET_DIR:-rust/target}.

set -euo pipefail

cd "$(dirname "$0")/.."

[ $# -eq 1 ] || { echo "usage: scripts/pack-build.sh <out-dir>" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "pack-build: Linux x86_64 only (this is $(uname -sm))" >&2; exit 1; }

# The glibc of the fleet's oldest host: a glibc binary runs only on a host whose glibc is at least the one it was
# linked against, and a glibc sotd that cannot start stops that host's mail. The floor is written in two places that
# change together: here, and release.yml's Linux smoke test, whose container (ubuntu:20.04, glibc 2.31) boots the artifact.
FLEET_GLIBC_FLOOR=2.31
if command -v x86_64-linux-musl-gcc >/dev/null || command -v musl-gcc >/dev/null; then
    triple=x86_64-unknown-linux-musl
    libc="musl (static)"
else
    host_glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | cut -d' ' -f2)" || host_glibc=""
    [ -n "$host_glibc" ] || { echo "pack-build: no musl C compiler and no readable glibc version on this host" >&2; exit 1; }
    [ "$(printf '%s\n%s\n' "$host_glibc" "$FLEET_GLIBC_FLOOR" | sort -V | tail -1)" = "$FLEET_GLIBC_FLOOR" ] \
        || { echo "pack-build: no musl C compiler, and this host's glibc $host_glibc is newer than the fleet's oldest ($FLEET_GLIBC_FLOOR), so a gnu sotd built here would not run everywhere: run it on a host with glibc <= $FLEET_GLIBC_FLOOR, e.g. the fleet's oldest glibc host" >&2; exit 1; }
    triple=x86_64-unknown-linux-gnu
    libc="gnu (glibc $host_glibc, runs on glibc >= $host_glibc)"
fi
[ -z "$(git status --porcelain)" ] || { echo "pack-build: the working tree is not clean; commit or stash first" >&2; exit 1; }
[ -n "$(git branch -r --contains HEAD)" ] \
    || { echo "pack-build: HEAD is on no remote-tracking branch; push the line first" >&2; exit 1; }

# A version is the release artifact iff it has no `+`: an ambient SOT_BUILD_ORIGIN=ci must not make a local pack one.
unset SOT_BUILD_ORIGIN

mkdir -p "$1"
out="$(cd "$1" && pwd)"
target="${CARGO_TARGET_DIR:-rust/target}"
mkdir -p "$target"
target="$(cd "$target" && pwd)"
commit="$(git rev-parse HEAD)"

# The same commands as release.yml's "Build release binaries" and "Build sotd + sot-capsule (musl static, linux)", with
# the gnu target when no musl compiler is present.
(
    cd rust
    export CARGO_TARGET_DIR="$target"
    cargo build --release -p sot-frontend --locked
    cargo build --release -p sot-backend -p sot-log --bin sotd --bin sot-capsule --target "$triple" --locked
)

# The backend folder: musl static or gnu, whichever the triple above chose.
be="$target/$triple/release"
v="$("$be/sotd" --version | cut -d' ' -f2)"
stage="sot-$v-linux-x86_64"
work="$(mktemp -d "${TMPDIR:-/tmp}/sot-pack.XXXXXX")"
trap 'rm -rf "${work:?}"' EXIT
mkdir "$work/$stage"
cp "$target/release/sot" "$be/sotd" "$be/sot-capsule" "$work/$stage/"
got="$("$work/$stage/sot" --version)"
case "$got" in "sot $v ("*) ;; *) echo "pack-build: VERSION MISMATCH for sot: $got (sotd says $v)" >&2; exit 1 ;; esac
cp deploy/sotd.service "$work/$stage/"
cp scripts/sot-apply.sh "$work/$stage/sot-apply"
tar -czf "$out/$stage.tar.gz" -C "$work" "$stage"
printf '%s\n' "$commit" > "$out/COMMIT"
printf '%s\n' "$libc" > "$out/LIBC"
(cd "$out" && sha256sum "$stage.tar.gz" COMMIT LIBC > SHA256SUMS)
echo "packed $v at $out (sotd: $libc)"
