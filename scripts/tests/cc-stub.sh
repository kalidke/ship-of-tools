#!/bin/sh
# cc-stub.sh: a stand-in C compiler for the gate's darwin-check, which type-checks for aarch64-apple-darwin from Linux,
# where no Apple clang exists. cargo check never links, so the C a build script compiles (ring's) only has to produce
# an object file for the archiver: this writes an empty file at the -o path (cc-rs passes it as -o, or as -Fo when it takes the name for an MSVC driver) and succeeds on every probe. Used only
# through CC_aarch64_apple_darwin in that step; no real build or shipped artifact sees it.
out=
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift ;;
    -Fo?*) out=${1#-Fo} ;;
  esac
  shift
done
[ -n "$out" ] && : > "$out"
exit 0
