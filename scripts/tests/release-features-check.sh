#!/usr/bin/env bash
# release-features-check.sh [--prove] [RUST_DIR]
#
# No shipped binary carries sot-chat's `test-support` feature: it builds the fake Slack and `Http::for_test`, and the
# fake's committed server key lets anyone impersonate its Slack hosts to a client that trusts the fixture's CA. Nor does
# one carry sot-protocol's `test-support` feature, which builds `ChatRecord::fixture`, test data in a shipped type. This
# resolves `--workspace` over the normal and build edges (`cargo tree -e normal,build`: the edges `cargo build --release`
# compiles, no dev-dependency's features) for the release targets, and fails when the resolved features of sot-chat or
# sot-protocol include `test-support`. One resolution covers every release build: under resolver 2 a package's
# features in any subset of the workspace are a subset of its features in the whole, and `--workspace` is itself the
# macOS and Windows selection.
# sot-chat's feature is also a compile error in a build without debug assertions (its lib.rs), so any release-profile
# build with it fails whatever the flag or manifest spelling; this check names the cause early, in the gate. A dev-dependency
# enabling the feature does not show here; the same line under [dependencies] of any crate (also by target table, in
# `default`, or another member) does.
# --prove also builds sot-chat in the release profile with the feature, which must fail with that compile error.
# --prove first runs the check against copies of the tree with each fault put in, each of which must end red, and the
# dev-dependency alone, which must end green. Needs the locked packages' sources for the release targets in CARGO_HOME
# (nothing is fetched; a depot missing them needs `cargo fetch --locked` once). Exit 0 ok; 1 a violation; 2 cargo or the arguments.
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
prove=0
[ "${1:-}" = --prove ] && { prove=1; shift; }
rust=$(cd "${1:-$here/../../rust}" && pwd) || exit 2
TARGETS=(x86_64-unknown-linux-gnu x86_64-unknown-linux-musl x86_64-pc-windows-msvc aarch64-apple-darwin)

# check <repo root> <cargo flags>
check() {
  local root=$1 flags=$2 t out targets=()
  for t in "${TARGETS[@]}"; do targets+=(--target "$t"); done
  # shellcheck disable=SC2086
  out=$(cargo tree --manifest-path "$root/rust/Cargo.toml" --workspace -e normal,build "${targets[@]}" -f '{p} [{f}]' $flags 2>&1) \
    || { echo "release-features-check: cargo tree failed (a depot missing the release targets' packages needs cargo fetch --locked once): $out" >&2; return 2; }
  for crate in sot-chat sot-protocol; do
    grep -qE "^[^a-z]*$crate v" <<< "$out" || { echo "release-features-check: $crate is not in the resolved tree" >&2; return 2; }
    if grep -qE "^[^a-z]*$crate v[^ ]* .*\[.*\btest-support\b" <<< "$out"; then
      echo "release-features-check: feature test-support of $crate reaches the workspace release builds (sot, sotd, sot-capsule); enabled through:" >&2
      # shellcheck disable=SC2086
      cargo tree --manifest-path "$root/rust/Cargo.toml" --workspace -e normal,build "${targets[@]}" -i $crate $flags 2>&1 | head -20 >&2
      return 1
    fi
  done
}

if [ "$prove" = 1 ]; then
  w=$(mktemp -d "${TMPDIR:-/tmp}/release-features-XXXXXX") || exit 2
  trap 'rm -rf "${w:?}"' EXIT
  root=$(dirname "$rust")
  # fault <name> <file under the repo root> <sed script, or empty> <message text it must end with, or ok> [setup command]
  fault() {
    local d=$w/$1 rc=0 msg
    mkdir -p "$d" && (cd "$root" && git ls-files -z --cached --others --exclude-standard rust | tar --null -T - -cf -) | tar -xf - -C "$d" || exit 2
    [ -z "$3" ] || { sed "$3" "$d/$2" > "$d/$2.new" && mv "$d/$2.new" "$d/$2"; } || exit 2
    [ -z "${5:-}" ] || (cd "$d" && eval "$5") || exit 2
    msg=$(check "$d" --offline 2>&1) || rc=$?
    if [ "$4" = ok ]; then [ "$rc" = 0 ] || { echo "FAIL: $1 was refused: $msg" >&2; exit 1; }
    else [ "$rc" = 1 ] && grep -q "$4" <<< "$msg" || { echo "FAIL: $1 not caught as expected (rc=$rc): $msg" >&2; exit 1; }; fi
    echo "PASS: $1"
  }
  CHAT='sot-chat = { path = "../chat", features = ["test-support"] }'
  RES='feature test-support of sot-chat reaches the workspace release builds'
  PROTO='sot-protocol = { path = "../protocol", features = ["test-support"] }'
  RESP='feature test-support of sot-protocol reaches the workspace release builds'
  fault dev-dependency-alone rust/backend/Cargo.toml '' ok
  fault feature-on-backend-normal-dependency rust/backend/Cargo.toml '0,/^sot-chat = { path = "..\/chat" }/s||'"$CHAT"'|' "$RES"
  fault protocol-feature-on-backend-normal-dependency rust/backend/Cargo.toml '0,/^sot-protocol = { path = "..\/protocol" }/s||'"$PROTO"'|' "$RESP"
  fault protocol-feature-in-default rust/protocol/Cargo.toml 's/^test-support = /default = ["test-support"]\ntest-support = /' "$RESP"
  fault feature-in-default rust/chat/Cargo.toml 's/^test-support = /default = ["test-support"]\ntest-support = /' "$RES"
  fault feature-on-frontend-normal-dependency rust/frontend/Cargo.toml '0,/^\[dependencies\]/s||[dependencies]\n'"$CHAT"'|' "$RES"
  fault feature-in-a-windows-target-table rust/backend/Cargo.toml "/^\[target.'cfg(windows)'.dependencies\]/a\\
$CHAT" "$RES"
  # a member no `-p` selection reaches, seen only by --workspace (the macOS and Windows release builds)
  EXTRA='[package]\nname = "extra"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\n'"$CHAT"'\n'
  fault feature-on-member-seen-only-by-workspace rust/Cargo.toml 's|^    "vt100",|    "vt100",\n    "extra",|' "$RES" \
    "mkdir -p rust/extra/src && : > rust/extra/src/lib.rs && printf '$EXTRA' > rust/extra/Cargo.toml"
  # The resolution cannot see `cargo build --release --workspace --all-features`; the compile error in sot-chat's lib.rs
  # does: a release-profile check of the crate with the feature must fail with that message.
  refused() {
    local msg
    msg=$(cd "$1" && cargo check --release -p sot-chat --features test-support --locked --offline 2>&1) \
      && { echo "FAIL: a release build of sot-chat with test-support was accepted" >&2; exit 1; }
    grep -q 'refused in a build without debug assertions' <<< "$msg" \
      || { echo "FAIL: the release build failed, but not with sot-chat's compile error: $(tail -5 <<< "$msg")" >&2; exit 1; }
    echo "PASS: a release build with test-support is a compile error"
  }
  refused "$rust"
fi

rc=0
check "$(dirname "$rust")" "--locked --offline" || rc=$?
[ "$rc" != 0 ] || echo "release-features-check: no shipped binary resolves test-support"
exit "$rc"
