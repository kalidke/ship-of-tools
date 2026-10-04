#!/usr/bin/env bash
# Self-test for moved-check.sh: tiny throwaway repos under mktemp -d.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tool="$here/moved-check.sh"
tmp="$(mktemp -d)"; trap 'rm -rf "${tmp:?}"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
pass=0; fail=0
G() { git -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@"; }
newrepo() { rm -rf "${tmp:?}/r"; mkdir "$tmp/r"; cd "$tmp/r"; G init -q -b main; }
commit() { G add -A; G commit -q -m "$1"; }
# run <name> <want-rc> [grep-patterns that must appear...]; range is base..head
check() {
  local name="$1" want="$2"; shift 2
  out="$("$tool" --repo "$tmp/r" HEAD~1..HEAD 2>&1)"; rc=$?
  local ok=1
  [ "$rc" = "$want" ] || ok=0
  for pat in "$@"; do grep -qF -- "$pat" <<<"$out" || ok=0; done
  if [ $ok = 1 ]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: $name (rc=$rc want $want)"; echo "$out"; fi
}

FN='fn alpha(x: i32) -> i32 {
    let y = x + 1;
    y * 2
}'
newrepo
printf '%s\n\nfn keep() {}\n' "$FN" > a.rs; commit base
printf 'fn keep() {}\n' > a.rs; printf '%s\n' "$FN" > b.rs; commit move
check "pure move" 0 "RESIDUAL-ADDED: 0" "RESIDUAL-REMOVED: 0" "MOVED: 4"

newrepo
printf '%s\n' "$FN" > a.rs; commit base
: > a.rs; printf '%s\n' "${FN/y \* 2/y * 3}" > b.rs; commit move
check "changed token" 1 "+ b.rs:3: y * 3" "- a.rs:3: y * 2" "RESIDUAL-ADDED: 1" "RESIDUAL-REMOVED: 1"

newrepo
printf '%s\n' "$FN" > a.rs; commit base
: > a.rs
printf 'use std::fmt;\nuse crate::{\n    a,\n    b,\n};\nimpl Foo {\n%s\n}\nimpl<T> Tr for Foo<T> {\n}\n' "$FN" > b.rs
printf 'mod b;\n#[cfg(test)]\npub mod c;\n' > lib.rs; commit move
check "scaffolding" 0 "RESIDUAL-ADDED: 0" "RESIDUAL-REMOVED: 0"

newrepo
printf '%s\n' "$FN" > a.rs; commit base
: > a.rs; printf '%s\n' "${FN//fn alpha/pub fn alpha}" > b.rs; commit move
check "visibility widening" 0 "MOVED: 4"

newrepo
printf '%s\n' "$FN" > a.rs; commit base
: > a.rs; printf 'impl X {\n%s\n}\n' "$(sed 's/^/    /' <<<"$FN")" > b.rs; commit move
check "re-indentation" 0 "MOVED: 4"

# a duplicate in the base: one copy moved, one deleted
newrepo
printf 'dup_line();\ndup_line();\n' > a.rs; commit base
printf 'dup_line();\n' > b.rs; : > a.rs; commit move
check "duplicate moved once deleted once" 1 "RESIDUAL-REMOVED: 1" "- a.rs:"

newrepo
echo x > a.txt; commit base
printf '# Page\n\n## Files\n- `a.txt`\n' > CLAUDE.md; commit page
check "new CLAUDE.md" 0 "SCAFFOLD: 4" "RESIDUAL-ADDED: 0"

newrepo
printf '#!/usr/bin/env bash\nhelper() {\n  echo hi\n}\n' > a.sh; commit base
printf '#!/usr/bin/env bash\n' > a.sh
printf '#!/usr/bin/env bash\n# moved from scripts/a.sh\nsource ./lib/b.sh\nhelper() {\n  echo hi\n}\n' > b.sh
printf 'source "$(dirname "$0")/b.sh"\n' >> a.sh; commit move
check "shell function with source" 0 "RESIDUAL-ADDED: 0" "RESIDUAL-REMOVED: 0"

newrepo
printf 'include("a.jl")\nf() = 1\n' > a.jl; commit base
printf 'module M\ninclude("b.jl")\nend\n' > a.jl; printf 'f() = 1\n' > b.jl; commit move
check "julia include and module" 0

newrepo
printf 'a\n' > a.txt; commit base
printf 'b\n' > a.txt; commit chg
printf 'b\n' > "$tmp/allow"
out="$("$tool" --repo "$tmp/r" --allow-file "$tmp/allow" HEAD~1..HEAD)"; rc=$?
if [ $rc = 1 ] && grep -qF "ALLOWED-BY-FILE: 1" <<<"$out"; then pass=$((pass+1)); else
  # pattern 'b' allows only the added line; removed 'a' remains residual
  fail=$((fail+1)); echo "FAIL: allow-file"; echo "$out"; fi
printf 'a\nb\n' > "$tmp/allow"
out="$("$tool" --repo "$tmp/r" --allow-file "$tmp/allow" HEAD~1..HEAD)"; rc=$?
if [ $rc = 0 ] && grep -qF "ALLOWED-BY-FILE: 2" <<<"$out"; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: allow-file both"; echo "$out"; fi

"$tool" --repo "$tmp/r" nonsense >/dev/null 2>&1; [ $? = 2 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: usage rc"; }
"$tool" --repo "$tmp/r" nope..nada >/dev/null 2>&1; [ $? = 2 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: git error rc"; }

echo "test-moved-check: $pass passed, $fail failed"
[ $fail = 0 ]
