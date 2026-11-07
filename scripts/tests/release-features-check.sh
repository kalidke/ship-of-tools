#!/usr/bin/env bash
# release-features-check.sh [--prove] [RUST_DIR]
#
# No shipped binary carries sot-chat's `test-support` feature (the fake Slack and `Http::for_test`, which trust a CA whose
# key is committed to the repository). For each release build command, with its package selection, this reads what Cargo
# resolves for the normal and build edges (`cargo tree -e normal,build`: the edges `cargo build --release` compiles, no
# dev-dependency's features) over every target, and fails when sot-chat's resolved features include `test-support`. A
# dev-dependency enabling it does not show; the same line under [dependencies] of any crate in the selection does.
# The selections are those of release.yml's and pack-build.sh's `cargo build --release` lines, and the check fails when
# a line there names another selection or any feature flag. Also: no include_str!/include_bytes! of a key or certificate
# outside the fake's folder. A re-export of `for_test` outside its cfg is not caught here (a review point, in the chat contract).
# --prove first runs the check against copies of the tree with each fault put in, and the dev-dependency alone; each must
# end as stated. Exit 0 ok; 1 a violation (the message names the feature and the binary); 2 cargo or the arguments.
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
prove=0
[ "${1:-}" = --prove ] && { prove=1; shift; }
rust=$(cd "${1:-$here/../../rust}" && pwd) || exit 2
SEL=("-p sot-frontend|sot" "-p sot-backend -p sot-capsule|sotd, sot-capsule" "--workspace|sot, sotd, sot-capsule")

# resolved <rust dir> <cargo flags>: 0 clean, 1 violation printed, 2 cargo failed
resolved() {
  local d=$1 flags=$2 s sel bins out
  for s in "${SEL[@]}"; do
    sel=${s%%|*} bins=${s#*|}
    # shellcheck disable=SC2086
    out=$(cargo tree --manifest-path "$d/Cargo.toml" $sel -e normal,build --target all -f '{p} [{f}]' $flags 2>&1) || { echo "release-features-check: cargo tree $sel failed: $out" >&2; return 2; }
    if grep -E '^[^a-z]*sot-chat v[^ ]* .*\[.*\btest-support\b' <<< "$out" > /dev/null; then
      echo "release-features-check: feature test-support of sot-chat is in the release build of $bins (cargo build --release $sel)" >&2
      return 1
    fi
  done
}

# static <repo root>: the release command lines and the compiled-in fixtures
static() {
  local root=$1 l n=0 bad=0
  while IFS= read -r l; do
    n=$((n + 1))
    grep -qE -- '(-p sot-frontend|--workspace|-p sot-backend -p sot-capsule --bin sotd)' <<< "$l" && ! grep -qE -- '--(all-)?features' <<< "$l" || { echo "release-features-check: a release build line names a selection or feature flag the check does not resolve: $l" >&2; bad=1; }
  done < <(grep -h 'cargo build --release' "$root/.github/workflows/release.yml" "$root/scripts/pack-build.sh" | grep -v '^[[:space:]]*#')
  [ "$n" -gt 0 ] || { echo "release-features-check: no 'cargo build --release' line found in release.yml or pack-build.sh" >&2; return 2; }
  if grep -rnE 'include_(str|bytes)!\(.*\.(pem|key)"' "$root/rust" --include=*.rs | grep -v '/chat/src/fake_slack/'; then
    echo "release-features-check: a key or certificate is compiled in outside the fake's folder" >&2; bad=1
  fi
  return $bad
}

if [ "$prove" = 1 ]; then
  w=$(mktemp -d "${TMPDIR:-/tmp}/release-features-XXXXXX") || exit 2
  trap 'rm -rf "${w:?}"' EXIT
  fault() { # <name> <file> <sed script> <expected bins text, or ok> [setup command]
    local d=$w/$1 rc=0 msg
    mkdir -p "$d" && (cd "$rust" && git ls-files -z --cached --others --exclude-standard . | tar --null -T - -cf -) | tar -xf - -C "$d" || exit 2
    [ -z "$3" ] || sed -i "$3" "$d/$2"
    [ -z "${5:-}" ] || (cd "$d" && eval "$5") || exit 2
    msg=$(resolved "$d" --offline 2>&1) || rc=$?
    if [ "$4" = ok ]; then [ "$rc" = 0 ] || { echo "FAIL: $1 was refused: $msg" >&2; exit 1; }
    else [ "$rc" = 1 ] && grep -q "test-support.*release build of $4 (" <<< "$msg" || { echo "FAIL: $1 not caught as expected (rc=$rc): $msg" >&2; exit 1; }; fi
    echo "PASS: $1"
  }
  CHAT='sot-chat = { path = "../chat", features = ["test-support"] }'
  fault dev-dependency-alone backend/Cargo.toml '' ok
  fault feature-on-backend-normal-dependency backend/Cargo.toml '0,/^sot-chat = { path = "..\/chat" }/s||'"$CHAT"'|' 'sotd, sot-capsule'
  fault feature-in-default chat/Cargo.toml 's/^test-support = /default = ["test-support"]\ntest-support = /' 'sotd, sot-capsule'
  fault feature-on-frontend-normal-dependency frontend/Cargo.toml '0,/^\[dependencies\]/s||[dependencies]\n'"$CHAT"'|' 'sot'
  # a member no `-p` selection reaches, seen only by --workspace (the macOS and Windows release builds)
  EXTRA='[package]\nname = "extra"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\n'"$CHAT"'\n'
  fault feature-on-member-seen-only-by-workspace Cargo.toml 's|^    "vt100",|    "vt100",\n    "extra",|' 'sot, sotd, sot-capsule' \
    "mkdir -p extra/src && : > extra/src/lib.rs && printf '$EXTRA' > extra/Cargo.toml"
fi

rc=0
resolved "$rust" "--locked --offline" || rc=$?
[ "$rc" != 0 ] || static "$(dirname "$rust")" || rc=1
[ "$rc" != 0 ] || echo "release-features-check: no shipped binary resolves test-support"
exit "$rc"
