#!/usr/bin/env bash
# Self-test for check-layout.sh: tiny throwaway repos under mktemp -d.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tool="${CHECK_LAYOUT_TOOL:-$here/check-layout.sh}"   # the override lets a case run against an older copy
tmp="$(mktemp -d)"; trap 'rm -rf "${tmp:?}"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
pass=0; fail=0
newrepo() { rm -rf "${tmp:?}/r"; mkdir "$tmp/r"; cd "$tmp/r"; git init -q -b main; }
commit() { git add -A; git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -q -m x; }
lines() { awk -v n="$1" -v p="$2" 'BEGIN{for(i=1;i<=n;i++) print p i}'; }
# check <name> <want-rc> <args...>; patterns come from $WANT (newline-separated), $NOT
check() {
  local name="$1" want="$2"; shift 2
  out="$("$tool" --repo "$tmp/r" --exempt /dev/null "$@" 2>&1)"; rc=$?
  local ok=1; [ "$rc" = "$want" ] || ok=0
  while IFS= read -r pat; do [ -z "$pat" ] || grep -qF -- "$pat" <<<"$out" || ok=0; done <<<"$WANT"
  while IFS= read -r pat; do [ -z "$pat" ] || ! grep -qF -- "$pat" <<<"$out" || ok=0; done <<<"$NOT"
  if [ $ok = 1 ]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: $name (rc=$rc want $want)"; echo "$out"; fi
  WANT=""; NOT=""
}
mkpage() { local d="$1"; shift; { printf '# P\n\n## Files\n'; for n in "$@"; do printf -- '- `%s` x\n' "$n"; done; printf '\n## Other\n- `bogus` y\n'; } > "$d/CLAUDE.md"; }

# clean repo
newrepo; mkdir d; lines 10 'x=' > d/a.jl; mkpage d a.jl; commit
WANT="violations: 0, allowed: 0, exempt: 0, unused-allow: 0, folders checked: 1"; check "clean" 0 d

# file-size
newrepo; mkdir d; lines 801 'x=' > d/a.jl; mkpage d a.jl; commit
WANT="VIOLATION file-size d/a.jl"; check "file-size fires" 1 d
printf '# c\nfile-size d/a.jl big on purpose\n' > "$tmp/allow"
WANT="ALLOWED file-size d/a.jl
violations: 0, allowed: 1"; check "file-size allowed" 0 --allow "$tmp/allow" d

# 700 code + 900 inline test lines passes
newrepo; mkdir d; { lines 700 '// c'; echo '#[cfg(test)]'; echo '#[allow(unused)]'; echo 'mod tests {'; lines 897 '// t'; } > d/a.rs; mkpage d a.rs; commit
WANT="violations: 0"; check "inline tests not counted" 0 d
# a test module in the middle of a file counts only its own span
newrepo; mkdir d; { lines 500 '// c'; echo '#[cfg(test)]'; echo 'mod t {'; lines 100 '    // t'; echo '}'; lines 400 '// d'; } > d/a.rs; mkpage d a.rs; commit
WANT="VIOLATION file-size d/a.rs 900 code lines"; check "mid-file test module" 1 d
out="$("$tool" --repo "$tmp/r" --report d)"
if grep -qF "code   900  test   103  a.rs" <<<"$out"; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: mid-file report"; echo "$out"; fi
# gated test modules: cfg(all(.., test, ..)) is a test module; any(..) and not(test) are code
tm() { # tm <name> <want-test-lines> <attr lines...>: 100 code lines then a gated module of 50 body lines
  local name="$1" want="$2"; shift 2
  newrepo; mkdir d; { lines 100 '// c'; for a in "$@"; do echo "$a"; done; echo 'mod t {'; lines 50 '    // t'; echo '}'; } > d/a.rs; mkpage d a.rs; commit
  out="$("$tool" --repo "$tmp/r" --report d)"
  local nattr=$#
  local expect_code=$((100)) expect_test=$((nattr + 52))
  if [ "$want" = 0 ]; then expect_code=$((100 + nattr + 52)); expect_test=0; fi
  if grep -qE "code +$expect_code +test +$expect_test +a.rs" <<<"$out"; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: gated module $name"; echo "$out"; fi
}
tm "all(test first)" 1 '#[cfg(all(test, target_os = "linux"))]'
tm "all(test last)" 1 '#[cfg(all(target_os = "linux", test))]'
tm "stacked cfg(test)+cfg(unix)" 1 '#[cfg(unix)]' '#[cfg(test)]'
tm "attributes between" 1 '#[cfg(test)]' '#[allow(dead_code)]' '#[path = "x.rs"]'
tm "all(any(a,b), test)" 1 '#[cfg(all(any(unix, windows), test))]'
tm "any(test, x) is code" 0 '#[cfg(any(test, feature = "x"))]'
tm "not(test) is code" 0 '#[cfg(not(test))]'
tm "all(unix, windows) is code" 0 '#[cfg(all(unix, windows))]'
# cfg(test) not followed by mod is not a test boundary
newrepo; mkdir d; { lines 700 '// c'; echo '#[cfg(test)]'; echo 'fn helper() {}'; lines 200 '// t'; } > d/a.rs; mkpage d a.rs; commit
WANT="VIOLATION file-size d/a.rs"; check "cfg(test) without mod" 1 d
# foo_tests.rs of 900 lines fails; tests/ folder too
newrepo; mkdir d; lines 900 '// t' > d/foo_tests.rs; mkpage d foo_tests.rs; commit
WANT="VIOLATION file-size d/foo_tests.rs"; check "_tests.rs 900" 1 d
newrepo; mkdir -p d/tests; lines 900 '// t' > d/tests/x.rs; mkpage d/tests x.rs; commit
WANT="VIOLATION file-size d/tests/x.rs"; check "tests/ folder 900" 1 d/tests
# any source under a tests/ folder is a test file: a .sh suite is not counted in folder-size, limit 800 total
newrepo; mkdir d; mkdir d/tests; for i in 1 2 3 4; do lines 790 'echo ' > d/tests/t$i.sh; done; mkpage d/tests t1.sh t2.sh t3.sh t4.sh; commit
WANT="violations: 0"; check ".sh under tests/ not counted in folder-size" 0 d/tests
newrepo; mkdir -p d/tests; lines 900 'echo ' > d/tests/big.sh; mkpage d/tests big.sh; commit
WANT="VIOLATION file-size d/tests/big.sh 900 test lines"; check ".sh under tests/ over 800" 1 d/tests
# extensionless shebang file counts as source
newrepo; mkdir d; { echo '#!/bin/sh'; lines 801 'echo '; } > d/run; mkpage d run; commit
WANT="VIOLATION file-size d/run"; check "extensionless shebang" 1 d

# no-page
newrepo; mkdir d; echo 'x=1' > d/a.jl; commit
WANT="VIOLATION no-page d "; check "no-page fires" 1 d
printf 'no-page d legacy\n' > "$tmp/allow"
WANT="ALLOWED no-page d"; check "no-page allowed" 0 --allow "$tmp/allow" d
# no source, no page needed
newrepo; mkdir d; echo hi > d/n.txt; commit
WANT="violations: 0"; check "no source no page" 0 d

# file-list: one missing, one extra; Other section ignored; subfolder entry
newrepo; mkdir -p d/sub; echo 'x=1' > d/a.jl; echo 'x=2' > d/b.jl; echo 'x=3' > d/sub/c.jl; echo n > d/notes.txt
mkpage d a.jl ghost.jl sub/ notes.txt; commit
WANT="VIOLATION file-list d/CLAUDE.md missing: b.jl; extra: ghost.jl"
NOT="bogus"; check "file-list missing+extra" 1 d
printf 'file-list d/CLAUDE.md in flux\n' > "$tmp/allow"
WANT="ALLOWED file-list d/CLAUDE.md"; check "file-list allowed" 0 --allow "$tmp/allow" d
# page written as in the common rules: colon after the backticks, folder with a slash
newrepo; mkdir -p d/sub; echo 'x=1' > d/a.jl; echo 'x=3' > d/sub/c.jl
printf '# P\n\n## Files\n- `a.jl`: code\n- `sub/`: folder\n' > d/CLAUDE.md; commit
WANT="violations: 0"; check "colon form with folder slash" 0 d
printf '# P\n\n## Files\n- `a.jl` code\n- `sub/` folder\n' > d/CLAUDE.md; commit
WANT="violations: 0"; check "no-colon form with folder slash" 0 d
printf '# P\n\n## Files\n- `a.jl/`: code\n- `sub`: folder\n' > d/CLAUDE.md; commit
WANT="missing: a.jl, sub/; extra: a.jl/, sub"; check "slash must match kind" 1 d
# tests_<subject>.rs and test_support.rs are test files
newrepo; mkdir d; lines 900 '// t' > d/tests_x.rs; lines 5 '//' > d/test_support.rs; lines 900 '// t' > d/a_tests_y.rs; mkpage d tests_x.rs test_support.rs a_tests_y.rs; commit
WANT="VIOLATION file-size d/tests_x.rs
VIOLATION file-size d/a_tests_y.rs"; NOT="folder-size"; check "tests_<subject> are test files" 1 d
newrepo; mkdir d; echo 'x=1' > d/a.jl; echo '# P' > d/CLAUDE.md; commit
WANT="no '## Files' section"; check "no Files section" 1 d
newrepo; mkdir d; echo 'x=1' > d/a.jl; mkpage d a.jl sub; commit
WANT="extra: sub"; check "empty subfolder is extra" 1 d

# folder-size: 13 files, and 3 x 1100... (use 4 x 790 code lines)
newrepo; mkdir d; for i in $(seq 1 13); do echo 'x=1' > d/f$i.jl; done; mkpage d $(cd d; ls *.jl); commit
WANT="VIOLATION folder-size d "; check "folder-size count" 1 d
printf 'folder-size d split pending\n' > "$tmp/allow"
WANT="ALLOWED folder-size d"; check "folder-size allowed" 0 --allow "$tmp/allow" d
newrepo; mkdir d; for i in 1 2 3 4; do lines 790 'x=' > d/f$i.jl; done; mkpage d f1.jl f2.jl f3.jl f4.jl; commit
WANT="VIOLATION folder-size d "; check "folder-size lines" 1 d
newrepo; mkdir d; for i in 1 2 3 4; do lines 790 'x=' > d/f${i}_tests.rs; done; mkpage d f1_tests.rs f2_tests.rs f3_tests.rs f4_tests.rs; commit
WANT="violations: 0"; check "test files not in folder-size" 0 d


# covered folders: crate root covers src/, Julia root covers src/ test/ ext/
newrepo; mkdir -p k/src/ops k/tests; printf '[package]\nname="k"\n' > k/Cargo.toml; echo 'fn a(){}' > k/src/lib.rs
echo 'fn b(){}' > k/src/ops/mod.rs; echo 'x' > k/src/ops/CLAUDE.md
printf '# P\n\n## Files\n- `Cargo.toml`: manifest\n- `src/lib.rs`: lib\n- `src/ops/`: ops\n' > k/CLAUDE.md; commit
WANT="violations: 0"; NOT="no-page"; check "crate root covers src" 0 k k/src
printf '# P\n\n## Files\n- `Cargo.toml`: manifest\n- `src/`: src\n' > k/CLAUDE.md; commit
WANT="VIOLATION file-list k/CLAUDE.md missing: src/lib.rs, src/ops/; extra: src/"; check "covered folder not listed itself" 1 k
git rm -q k/CLAUDE.md; commit
WANT="VIOLATION no-page k covers k/src, no CLAUDE.md"; check "covered folder needs the root page" 1 k
newrepo; mkdir -p p/src p/test p/ext p/test/fixtures; printf 'name="p"\n' > p/Project.toml; echo 'x=1' > p/src/P.jl; echo 'x=1' > p/test/runtests.jl; echo 'x=1' > p/ext/E.jl; echo 'x=1' > p/test/fixtures/f.jl
printf '# P\n\n## Files\n- `Project.toml` m\n- `src/P.jl` a\n- `test/runtests.jl` b\n- `test/fixtures/` c\n- `ext/E.jl` d\n' > p/CLAUDE.md; commit
WANT="VIOLATION no-page p/test/fixtures
violations: 1,"; check "julia root covers src test ext" 1
printf 'no-page **/test/*/** E4 fixtures\n' > "$tmp/ex"
WANT="violations: 0, allowed: 0, exempt: 1,"; check "exempt glob silences" 0 --exempt "$tmp/ex"
# a covered folder with its own page is an ordinary folder
newrepo; mkdir -p p/src; printf 'name="p"\n' > p/Project.toml; echo 'x=1' > p/src/P.jl; printf '# P\n\n## Files\n- `Project.toml` m\n- `src/` s\n' > p/CLAUDE.md
printf '# S\n\n## Files\n- `P.jl` a\n' > p/src/CLAUDE.md; commit
WANT="violations: 0"; check "covered folder with own page" 0 p p/src
# exempt kinds and the default exempt.txt beside the script
newrepo; mkdir -p dev/x; echo 'x=1' > dev/x/a.jl; commit
out="$("$tool" --repo "$tmp/r" 2>&1)"; rc=$?
if [ $rc = 0 ] && grep -qF "exempt: 1," <<<"$out"; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: default exempt.txt"; echo "$out"; fi
"$tool" --repo "$tmp/r" --exempt "$tmp/nonexistent" >/dev/null 2>&1; [ $? = 2 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: missing --exempt rc"; }
newrepo; mkdir -p rust/vt100/src; for i in 1 2 3 4; do lines 790 'x=' > rust/vt100/src/f$i.rs; done; lines 900 'x=' > rust/vt100/src/big.rs; printf '[package]\n' > rust/vt100/Cargo.toml; printf '# P\n\n## Files\n- `Cargo.toml` m\n- `src/f1.rs` a\n- `src/f2.rs` a\n- `src/f3.rs` a\n- `src/f4.rs` a\n- `src/big.rs` a\n' > rust/vt100/CLAUDE.md; commit
WANT="VIOLATION file-size
VIOLATION folder-size"; check "vt100 violations without exemption" 1 rust/vt100/src
out="$("$tool" --repo "$tmp/r" rust/vt100/src 2>&1)"; rc=$?
if [ $rc = 0 ]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: vt100 exempt by default"; echo "$out"; fi

# unused allow lines fail
newrepo; mkdir d; lines 10 'x=' > d/a.jl; mkpage d a.jl; commit
printf 'file-size d/a.jl stale\nno-page d also stale\n' > "$tmp/allow"
WANT="UNUSED-ALLOW file-size d/a.jl stale
UNUSED-ALLOW no-page d also stale
unused-allow: 2"; check "unused allow lines" 1 --allow "$tmp/allow" d
# a folder with a page but no source is checked by default (and lists its entries)
newrepo; mkdir -p w; echo 'on: push' > w/ci.yml; echo 'on: pr' > w/old.yml; printf '# W\n\n## Files\n- `ci.yml` a\n- `gone.yml` b\n' > w/CLAUDE.md; commit
WANT="VIOLATION file-list w/CLAUDE.md missing: old.yml; extra: gone.yml"; check "page-only folder checked by default" 1
# default: every folder with source; detached worktree
newrepo; mkdir -p a b; echo 'x=1' > a/x.jl; echo 'x=1' > b/y.jl; mkpage a x.jl; commit
WANT="VIOLATION no-page b 
folders checked: 2"; check "all folders" 1
git worktree add -q --detach "$tmp/wt" HEAD
out="$("$tool" --repo "$tmp/wt" 2>&1)"; rc=$?
if [ $rc = 1 ] && grep -qF "no-page b" <<<"$out"; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: detached worktree"; echo "$out"; fi

# named-path: the backticked repo paths of docs/ownership.md must exist (outside fenced blocks, placeholders skipped)
np() { # np <page text...>: a repo with folder d (a.jl) and docs/ownership.md holding the given lines
  newrepo; mkdir d docs; lines 3 'x=' > d/a.jl; mkpage d a.jl; printf '%s\n' "$@" > docs/ownership.md; commit
}
np 'a file `d/a.jl` and a line form `d/a.jl:3-5` and an item form `d/a.jl::item`'
WANT="named-path: 3 tokens checked in 1 files
violations: 0"; check "named-path present" 0 d
np 'a missing file `d/gone.jl`'
WANT="VIOLATION named-path docs/ownership.md d/gone.jl"; check "named-path missing" 1 d
np 'a placeholder `d/<name>.jl`, a glob `d/*.jl`, a var `$HOME/d/x`, a tilde `~/d/x`, a spaced `d/a b`'
WANT="named-path: 0 tokens checked
violations: 0"; check "named-path placeholders skipped" 0 d
np 'a brace group `d/{a,gone}.jl`'
WANT="VIOLATION named-path docs/ownership.md d/gone.jl"; NOT="VIOLATION named-path docs/ownership.md d/a.jl"; check "named-path brace group" 1 d
np 'before' '```' 'a fenced `d/gone.jl`' '```' 'after `d/a.jl`'
WANT="named-path: 1 tokens checked
violations: 0"; check "named-path fenced ignored" 0 d
np 'an untracked top-level word `nothere/x.jl` and a folder `d`'
WANT="named-path: 1 tokens checked
violations: 0"; check "named-path unknown top-level skipped" 0 d
np 'a page'; printf '%s\n' 'a missing `d/gone.jl`' > docs/integration.md; printf '%s\n' 'a missing `d/gone.jl`' > CLAUDE.md; commit
WANT="VIOLATION named-path docs/integration.md d/gone.jl
VIOLATION named-path CLAUDE.md d/gone.jl
named-path: 2 tokens checked in 3 files"; check "named-path lists the integration page and the root page" 1 d

# too-many-lines: the allowances under rust/ (not rust/vt100) equal the pin; a rise and a fall each fail and say which
newrepo; mkdir -p rust/a rust/vt100
printf '#[allow(clippy::too_many_lines, reason = "x")]\nfn f() {}\n' > rust/a/x.rs; mkpage rust/a x.rs
printf '#[allow(clippy::too_many_lines)]\n' > rust/vt100/v.rs; mkpage rust/vt100 v.rs; commit
printf 'too-many-lines rust 1 pinned\n' > "$tmp/allow"
WANT="too-many-lines: 1 allowances in rust (pinned: 1)
violations: 0, allowed: 0, exempt: 0, unused-allow: 0"; check "too-many-lines at its pin" 0 --allow "$tmp/allow" rust/a
printf 'too-many-lines rust 0 pinned\n' > "$tmp/allow"
WANT="VIOLATION too-many-lines rust 1 allowances, pinned 0: the count rose"; check "too-many-lines rise" 1 --allow "$tmp/allow" rust/a
WANT="VIOLATION too-many-lines rust 1 allowances, pinned 0: the count rose"; check "too-many-lines unpinned" 1 rust/a
printf 'too-many-lines rust 2 pinned\n' > "$tmp/allow"
WANT="VIOLATION too-many-lines rust 1 allowances, pinned 2: the count fell; lower the pin to 1"; check "too-many-lines fall" 1 --allow "$tmp/allow" rust/a
printf 'too-many-lines rust many pinned\n' > "$tmp/allow"
"$tool" --repo "$tmp/r" --exempt /dev/null --allow "$tmp/allow" rust/a >/dev/null 2>&1; [ $? = 2 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: too-many-lines bad pin rc"; }

# report
newrepo; mkdir d; { lines 5 'x='; } > d/small.rs; { lines 30 'x='; echo '#[cfg(test)]'; echo 'mod t {'; echo '}'; } > d/big.rs; commit
out="$("$tool" --repo "$tmp/r" --report d)"
if [ "$(sed -n 2p <<<"$out")" = "  code    30  test     3  big.rs" ]; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL: report"; echo "$out"; fi

"$tool" --repo "$tmp/r" --bogus >/dev/null 2>&1; [ $? = 2 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: usage rc"; }
"$tool" --repo "$tmp" >/dev/null 2>&1; [ $? = 2 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: not a repo rc"; }

echo "test-check-layout: $pass passed, $fail failed"
[ $fail = 0 ]
