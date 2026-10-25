#!/bin/bash
. "${BASH_SOURCE[0]%/*}/publish-guard-fixture.sh"
fixture setup
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --require-ready > "$T/setup.out"
grep -F 'policy ready' "$T/setup.out" > /dev/null || fail 'readiness did not validate list'
git -C "$R" config --unset sot.publishList
if "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --require-ready > "$T/setup.out" 2>&1; then fail 'missing policy reported ready'; fi
grep -F 'sot.publishList' "$T/setup.out" > /dev/null || fail 'readiness wrong cause'
git -C "$R" config sot.publishList "$POLICY"
ok 'placement and readiness have distinct outcomes'
printf new > "$R/data"; commit
# The external list is validated afresh on every push, before any chained hook.
for location in unset missing relative empty multiple directory worktree metadata unrelated bare file-link directory-link; do
    git -C "$R" config --local --unset-all sot.publishList || true
    case "$location" in
        unset) ;;
        missing) git -C "$R" config sot.publishList "$T/absent" ;;
        relative) git -C "$R" config sot.publishList ../publish-list.txt ;;
        empty) git -C "$R" config sot.publishList '' ;;
        multiple) git -C "$R" config --add sot.publishList "$POLICY"; git -C "$R" config --add sot.publishList "$POLICY" ;;
        directory) git -C "$R" config sot.publishList "$T" ;;
        worktree) cp "$POLICY" "$R/inside-list"; git -C "$R" config sot.publishList "$R/inside-list" ;;
        metadata) cp "$POLICY" "$R/.git/inside-list"; git -C "$R" config sot.publishList "$R/.git/inside-list" ;;
        unrelated) git init -q "$T/unrelated"; cp "$POLICY" "$T/unrelated/list"; git -C "$R" config sot.publishList "$T/unrelated/list" ;;
        bare) cp "$POLICY" "$B/list"; git -C "$R" config sot.publishList "$B/list" ;;
        file-link) ln -s "$R/inside-list" "$T/linked-list"; git -C "$R" config sot.publishList "$T/linked-list" ;;
        directory-link) ln -s "$R" "$T/linked-dir"; git -C "$R" config sot.publishList "$T/linked-dir/inside-list" ;;
    esac
    reject "list validation $location" 'sot.publishList' origin main
done
git -C "$R" config --local --replace-all sot.publishList "$POLICY"
accept 'valid external list after invalid controls' origin main
# Only the common local raw setting counts, not global or command configuration.
git -C "$R" config --local --unset-all sot.publishList
printf '[sot]\n    publishList = %s\n' "$POLICY" > "$T/global-config"
GIT_CONFIG_GLOBAL="$T/global-config" reject 'global policy is not a fallback' 'sot.publishList' origin HEAD:refs/heads/global
if git -C "$R" -c sot.publishList="$POLICY" push -q origin HEAD:command > "$T/push.out" 2>&1; then fail 'command policy accepted'; fi
grep -F 'sot.publishList' "$T/push.out" > /dev/null || fail 'command policy wrong refusal'
ok 'command configuration ignored'
# Ignored system config is also insufficient, and a worktree-local policy cannot replace the common setting.
GIT_CONFIG_SYSTEM="$T/global-config" GIT_CONFIG_NOSYSTEM=0 reject 'system policy is not a fallback' 'sot.publishList' origin HEAD:refs/heads/system

mkdir "$T/policy-parent"
cp "$POLICY" "$T/policy-parent/list"
git -C "$R" config --local sot.publishList "$T/policy-parent/list"
accept 'external policy parent starts ready' origin HEAD:refs/heads/policy-parent
git init -q "$T/policy-parent"
reject 'policy parent becoming a repository is detected on next push' 'sot.publishList' origin HEAD:refs/heads/policy-parent-now-git
git -C "$R" config --local sot.publishList "$POLICY"
# Remove the intentional inside-repository policy fixture before staging later controls.
rm -f "$R/inside-list"
# CRLF, unterminated last lines, literal glob characters, and literal permitted spans.
printf '# comment\r\n\r\nA[bc]*?\r\n!A[bc]*?.example/ok' > "$POLICY"
printf 'a[BC]*?.example/ok\n' > "$R/data"; commit
accept 'CRLF and unterminated literal allowance' origin main
printf 'a[BC]*?\n' > "$R/data"; commit
reject 'literal metacharacters' 'line 3' origin main
printf '%s\n' '# synthetic policy' '' 'glimmerdeny' 'otherdeny' '!glimmerdeny.example/ok' > "$POLICY"
git -C "$R" reset -q --hard origin/main
# Effective hook conflicts and occupied slots are preserved before setup does substantive work.
fixture conflicts
printf '#!/bin/sh\nexit 17\n' > "$T/foreign-hook"
mkdir "$T/custom-hooks"
cp "$T/foreign-hook" "$T/custom-hooks/pre-push"
git -C "$R" config core.hooksPath "$T/custom-hooks"
if "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out" 2>&1; then fail 'foreign hooksPath overwritten'; fi
[ "$(git -C "$R" config core.hooksPath)" = "$T/custom-hooks" ] || fail 'foreign config changed'
cmp "$T/foreign-hook" "$T/custom-hooks/pre-push" || fail 'foreign hook changed'
grep -F "$R" "$T/setup.out" > /dev/null && grep -F 'config --local --unset-all core.hooksPath' "$T/setup.out" > /dev/null || fail 'migration instruction missing'
ok 'foreign hooksPath preserved and clone named'
git -C "$R" config --unset core.hooksPath
for choice in '' "$R/.git/hooks"; do
    git -C "$R" config core.hooksPath "$choice"
    if "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out" 2>&1; then fail 'explicit hooksPath accepted'; fi
    git -C "$R" config --unset core.hooksPath
done
ok 'explicit empty and explicit default choices preserved'
printf '[core]\n    hooksPath = %s\n' "$T/custom-hooks" > "$T/global-hooks"
if GIT_CONFIG_GLOBAL="$T/global-hooks" "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out" 2>&1; then fail 'global hooks override accepted'; fi
ok 'global hook selection preserved'

cp "$R/.git/hooks/pre-push" "$T/owned-shim"
printf '#!/bin/sh\nexit 0\n' > "$R/.git/hooks/pre-push"
if "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out" 2>&1; then fail 'modified owned slot overwritten'; fi
grep -F 'locally modified' "$T/setup.out" > /dev/null || fail 'modified slot wrong cause'
ok 'locally modified owned hook preserved'
mv "$T/owned-shim" "$R/.git/hooks/pre-push"; chmod +x "$R/.git/hooks/pre-push"
rm -f "$R/.git/publish-guard/installed-hash"
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out"
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out"
ok 'exact shipped shim recovers interrupted record and repeats'
cp "$R/.git/hooks/pre-push" "$T/shim-copy"
rm -f "$R/.git/hooks/pre-push"
ln -s "$T/shim-copy" "$R/.git/hooks/pre-push"
if "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out" 2>&1; then fail 'linked slot followed'; fi
ok 'symlink slot refused'
rm -f "$R/.git/hooks/pre-push"
printf 'foreign nonexecutable\n' > "$R/.git/hooks/pre-push"
if "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out" 2>&1; then fail 'unknown nonexecutable hook overwritten'; fi
ok 'unknown nonexecutable hook preserved'
# An exact owned shim never authorizes following a linked ownership record.
rm -f "$R/.git/hooks/pre-push"
cp "$T/shim-copy" "$R/.git/hooks/pre-push"; chmod +x "$R/.git/hooks/pre-push"
mv "$R/.git/publish-guard/installed-hash" "$T/hash-record"
ln -s "$T/hash-record" "$R/.git/publish-guard/installed-hash"
if "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out" 2>&1; then fail 'linked ownership record accepted'; fi
ok 'linked ownership record preserved'
fixture worktrees
# A linked worktree with no guard is still covered by the common shim.
blob=$(printf clean | git -C "$R" hash-object -w --stdin)
tree=$(printf '100644 blob %s\tdata\n' "$blob" | git -C "$R" mktree)
old=$(printf old | git -C "$R" commit-tree "$tree")
W="$T/older worktree"
git -C "$R" worktree add -q --detach "$W" "$old"
main_R=$R; R=$W
reject 'old worktree missing guard' 'checked-out publish guard is missing' origin HEAD:refs/heads/old-worktree
R=$main_R
# Distinct checked-out versions run from each pushing worktree.
W2="$T/current worktree"
git -C "$R" worktree add -q --detach "$W2" HEAD
{ IFS= read -r first; printf '%s\nprintf "version marker\\n" >&2\n' "$first"; cat; } < "$W2/scripts/hooks/pre-push" > "$T/variant"
cp "$T/variant" "$W2/scripts/hooks/pre-push"; chmod +x "$W2/scripts/hooks/pre-push"
R=$W2
accept 'linked worktree uses its checked-out guard' origin HEAD:refs/heads/versioned
grep -F 'version marker' "$T/push.out" > /dev/null || fail 'wrong guard version'
R=$main_R
git -C "$W2" config extensions.worktreeConfig true
git -C "$W2" config --worktree sot.publishList "$T/missing-worktree-policy"
R=$W2
accept 'worktree policy does not override common local value' origin HEAD:refs/heads/common-policy
git -C "$main_R" config --local --unset sot.publishList
git -C "$W2" config --worktree sot.publishList "$POLICY"
reject 'worktree policy is not a fallback' 'sot.publishList' origin HEAD:refs/heads/worktree-policy
git -C "$main_R" config --local sot.publishList "$POLICY"
R=$main_R
git -C "$W2" config --worktree core.hooksPath "$T/custom-hooks"
if "$BASH" "$R/scripts/setup-publish-guard.sh" "$W2" > "$T/setup.out" 2>&1; then fail 'worktree override accepted'; fi
ok 'worktree override conflict'
git -C "$W2" config --worktree --unset core.hooksPath
# Missing/invalid lists refuse before the optional local hook, including deletion-only pushes.
fixture chaining
CHAIN="$R/.git/hooks/pre-push.local"
cat > "$CHAIN" <<'CHAIN'
#!/bin/sh
printf '%s\n' "$#" "$1" "$2" > "$HOME/chain-args"
cat > "$HOME/chain-input"
printf 'chain stdout\n'
printf 'chain ran\n' >&2
exit 0
CHAIN
chmod +x "$CHAIN"
printf clean2 > "$R/data"; commit
head=$(git -C "$R" rev-parse HEAD)
old=$(git -C "$B" rev-parse main)
zero_oid=$(printf '%*s' "${#head}" '' | tr ' ' 0)
git -C "$R" tag clean-tag
# A transparent fixture recorder captures exactly what Git supplied before the shipped shim/guard run.
cp "$R/.git/hooks/pre-push" "$HOME/original-shim"
cat > "$R/.git/hooks/pre-push" <<'RECORDER'
#!/bin/sh
cat > "$HOME/guard-input"
printf '%s\n' "$#" "$1" "$2" > "$HOME/guard-args"
exec "$HOME/original-shim" "$@" < "$HOME/guard-input"
RECORDER
chmod +x "$HOME/original-shim" "$R/.git/hooks/pre-push"
accept 'passing chain receives multi-update push' origin main clean-tag
cmp "$HOME/guard-input" "$HOME/chain-input" || fail 'chain changed stdin bytes or order'
cmp "$HOME/guard-args" "$HOME/chain-args" || fail 'chain changed original arguments'
grep -F 'chain stdout' "$T/push.out" > /dev/null && grep -F 'chain ran' "$T/push.out" > /dev/null || fail 'chain streams were not forwarded'
cp "$HOME/original-shim" "$R/.git/hooks/pre-push"; chmod +x "$R/.git/hooks/pre-push"
printf '2\norigin\n%s\n' "$B" > "$T/expected-args"
cmp "$T/expected-args" "$HOME/chain-args" || fail 'chain arguments changed'
# Git may order updates by ref; compare complete newline-terminated records in sorted order.
printf 'refs/heads/main %s refs/heads/main %s\nrefs/tags/clean-tag %s refs/tags/clean-tag %s\n' "$head" "$old" "$head" "$zero_oid" | sort > "$T/expected-input"
sort "$HOME/chain-input" > "$T/actual-input"
cmp "$T/expected-input" "$T/actual-input" || fail 'chain input changed'
[ "$(tail -c 1 "$HOME/chain-input" | od -An -tu1 | tr -d ' ')" = 10 ] || fail 'chain lost trailing newline'
ok 'exact chain arguments and stdin'
rm -f "$HOME/chain-input"
printf glimmerdeny > "$R/data"; commit
reject 'denial precedes chain' 'line 3' origin main
[ ! -e "$HOME/chain-input" ] || fail 'denied push reached chain'
git -C "$R" reset -q --hard origin/main
printf clean3 > "$R/data"; commit
mv "$POLICY" "$T/policy-for-chain"
reject 'missing policy precedes local hook' 'sot.publishList' origin main
[ ! -e "$HOME/chain-input" ] || fail 'missing policy reached chain'
mv "$T/policy-for-chain" "$POLICY"
printf '#!/bin/sh\ncat >/dev/null\necho "pre-push: local veto" >&2\nexit 23\n' > "$CHAIN"
reject 'local veto propagates' 'local veto' origin main
chain_status=0
(cd "$R" && "$BASH" .git/hooks/pre-push origin "$B" < "$HOME/guard-input") > "$T/direct-chain.out" 2>&1 || chain_status=$?
[ "$chain_status" = 23 ] || fail 'guard lost the exact chained status'
ok 'exact chained status'

cp "$R/scripts/hooks/pre-push-shim" "$CHAIN"; chmod +x "$CHAIN"
reject 'chain recursion refuses' 'hook chain cycle' origin main
rm -f "$CHAIN"
printf '#!/bin/sh\necho veto >&2\nexit 29\n' > "$R/.git/hooks/pre-commit"; chmod +x "$R/.git/hooks/pre-commit"
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" > "$T/setup.out"
if git -C "$R" commit -q --allow-empty -m control > "$T/commit.out" 2>&1; then fail 'default pre-commit disabled'; fi
grep -F veto "$T/commit.out" > /dev/null || fail 'pre-commit not reached'
ok 'default pre-commit preserved'
# Compatibility for a direct external guard retains the default-slot hook's veto.
fixture external-chain
git -C "$R" config core.hooksPath "$R/scripts/hooks"
printf '#!/bin/sh\ncat >/dev/null\necho "pre-push: default veto" >&2\nexit 19\n' > "$R/.git/hooks/pre-push"
chmod +x "$R/.git/hooks/pre-push"
reject 'external guard preserves distinct default hook' 'default veto' origin HEAD:refs/heads/external-veto
printf '#!/bin/sh\ncat > "$HOME/external-input"\nexit 0\n' > "$R/.git/hooks/pre-push"
accept 'external guard passing default hook' origin HEAD:refs/heads/external-pass
[ -s "$HOME/external-input" ] || fail 'default hook lost input'
ok 'external guard keeps stdin'
rm -f "$R/.git/hooks/pre-push"
ln "$R/scripts/hooks/pre-push" "$R/.git/hooks/pre-push"
accept 'external guard cannot chain to its own file identity' origin HEAD:refs/heads/no-self-chain

# A fresh raw clone is unguarded until the supported setup entry enrolls it.
fixture enrollment
C="$T/fresh clone"
git clone -q -b main "$B" "$C"
R=$C
printf glimmerdeny > "$R/data"; commit
accept 'never-enrolled clone limit' origin HEAD:refs/heads/before-setup
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --list "$POLICY" > "$T/setup.out"
reject 'same clone protected after setup' 'line 3' origin HEAD:refs/heads/after-setup
# Autocrlf checkout uses LF shebangs because the attributes travel with the source.
fixture line-endings
cp "$ROOT/.gitattributes" "$R/.gitattributes"; commit
accept 'attributes fixture' origin main
C="$T/autocrlf"
git -c core.autocrlf=true clone -q -b main "$B" "$C"
R=$C
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --list "$POLICY" > "$T/setup.out"
printf glimmerdeny > "$R/data"; commit
reject 'autocrlf true launches shipped hook' 'line 3' origin main
# Assert the actual interpreter Git launches, not just the harness shell.
cat > "$R/.git/hooks/pre-push.local" <<'PROBE'
#!/bin/bash
printf '%s\n' "$BASH_VERSION" > "$HOME/interpreter"
cat >/dev/null
PROBE
if [ -n "${HOOK_SHELL:-}" ]; then
    { printf '#!%s\n' "$HOOK_SHELL"; tail -n +2 "$R/.git/hooks/pre-push.local"; } > "$T/probe"
    cp "$T/probe" "$R/.git/hooks/pre-push.local"
fi
chmod +x "$R/.git/hooks/pre-push.local"
git -C "$R" reset -q --hard origin/main
accept 'Git-launched interpreter probe' origin HEAD:refs/heads/interpreter
[ -s "$HOME/interpreter" ] || fail 'Git-launched hook interpreter was not recorded'
case "$(uname -s)" in Darwin) case "$(cat "$HOME/interpreter")" in 3.2.*) ;; *) fail 'hook did not run under macOS bash 3.2' ;; esac ;; esac
if [ -n "${HOOK_SHELL:-}" ]; then
    [ "$(cat "$HOME/interpreter")" = "$("$HOOK_SHELL" -c 'printf "%s\n" "$BASH_VERSION"')" ] || fail 'hook interpreter differs from requested shell'
fi
# Native Windows list spelling and directory junction rejection.
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*)
    git -C "$R" config sot.publishList "$(cygpath -aw "$POLICY")"
    accept 'native Windows absolute list path' origin HEAD:refs/heads/native-path
    mkdir "$T/policy-directory"
    cp "$POLICY" "$T/policy-directory/publish-list.txt"
    SOT_TEST_JUNCTION="$(cygpath -aw "$T/junction")" SOT_TEST_TARGET="$(cygpath -aw "$T/policy-directory")" powershell.exe -NoProfile -NonInteractive -Command 'New-Item -ItemType Junction -Path $env:SOT_TEST_JUNCTION -Target $env:SOT_TEST_TARGET | Out-Null' > /dev/null
    git -C "$R" config sot.publishList "$T/junction/publish-list.txt"
    reject 'Windows reparse directory refuses' 'sot.publishList' origin HEAD:refs/heads/junction
    ;; esac
# A load failure cannot leave a permissive scanner running without its validators.
fixture validator-load
printf glimmerdeny > "$R/data"; commit
VALIDATOR="$R/scripts/hooks/publish-guard-lib.sh"
cp "$VALIDATOR" "$T/validator-original"
restore_validator_access() {
    case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*)
        [ -f "$T/validator-acl" ] || return 0
        SOT_TEST_VALIDATOR="$(cygpath -aw "$VALIDATOR")" SOT_TEST_ACL="$(cygpath -aw "$T/validator-acl")" \
            powershell.exe -NoProfile -NonInteractive -Command '
                $ErrorActionPreference = "Stop"
                $a = Get-Acl -LiteralPath $env:SOT_TEST_VALIDATOR
                $a.SetSecurityDescriptorSddlForm([IO.File]::ReadAllText($env:SOT_TEST_ACL), [Security.AccessControl.AccessControlSections]::Access)
                Set-Acl -LiteralPath $env:SOT_TEST_VALIDATOR -AclObject $a' || return 1 ;;
        *) chmod u+rw "$VALIDATOR" ;;
    esac
}
trap 'restore_validator_access; rm -rf "${T:?}"' EXIT
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*)
    SOT_TEST_VALIDATOR="$(cygpath -aw "$VALIDATOR")" SOT_TEST_ACL="$(cygpath -aw "$T/validator-acl")" \
        powershell.exe -NoProfile -NonInteractive -Command '
            $ErrorActionPreference = "Stop"
            $a = Get-Acl -LiteralPath $env:SOT_TEST_VALIDATOR
            [IO.File]::WriteAllText($env:SOT_TEST_ACL, $a.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access))
            $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
            $r = [Security.AccessControl.FileSystemAccessRule]::new($sid, "ReadData", "Deny")
            $a.AddAccessRule($r)
            Set-Acl -LiteralPath $env:SOT_TEST_VALIDATOR -AclObject $a' || fail 'cannot deny scratch validator reads'
    ;;
    *) chmod 000 "$VALIDATOR" ;;
esac
if "$BASH" -c '. "$1"' fixture "$VALIDATOR" > "$T/source.out" 2>&1; then fail 'unreadable validator fixture is still readable'; fi
reject 'unreadable validator refuses denied push' 'cannot load policy validator' origin main
not_ready 'unreadable validator' 'cannot load policy validator'
restore_validator_access || fail 'cannot restore scratch validator access'
trap 'rm -rf "${T:?}"' EXIT
cp "$T/validator-original" "$VALIDATOR"
for fault in source-status incomplete list-status encoding-status alias-status; do
    cp "$T/validator-original" "$VALIDATOR"
    case "$fault" in
        source-status) printf '\nreturn 73\n' >> "$VALIDATOR"; reason='cannot load policy validator'; ready_reason=$reason ;;
        incomplete) : > "$VALIDATOR"; reason='policy validator is incomplete'; ready_reason=$reason ;;
        list-status) printf '\npg_list() { return 73; }\n' >> "$VALIDATOR"; reason='policy validation failed'; ready_reason=$reason ;;
        encoding-status) printf '\npg_list_encoding() { return 73; }\n' >> "$VALIDATOR"; reason='encoding validation failed'; ready_reason=$reason ;;
        alias-status) printf '\npg_no_reparse() { return 73; }\n' >> "$VALIDATOR"; reason='unresolved directory alias'; ready_reason='unresolved alias' ;;
    esac
    reject "validator failure $fault" "$reason" origin main
    not_ready "validator failure $fault" "$ready_reason"
done
cp "$T/validator-original" "$VALIDATOR"
reject 'restored validator still denies content' 'line 3' origin main
reset_case
printf 'validator control\n' > "$R/data"; commit
accept 'restored validator accepts clean update' origin main

# Validate the entire UTF-8 file, including ignored comments, before matching.
fixture encodings
printf glimmerdeny > "$R/data"; commit
for encoding in utf8-bom utf16le utf16be utf16le-no-bom utf16be-no-bom nul nul-comment \
    invalid-lead stray-continuation bad-continuation truncated-two truncated-three truncated-four \
    overlong-two overlong-three overlong-four surrogate above-unicode invalid-comment; do
    case "$encoding" in
        utf8-bom) printf '\357\273\277glimmerdeny\n' > "$POLICY" ;;
        utf16le) printf '\377\376g\000l\000i\000m\000m\000e\000r\000d\000e\000n\000y\000\n\000' > "$POLICY" ;;
        utf16be) printf '\376\377\000g\000l\000i\000m\000m\000e\000r\000d\000e\000n\000y\000\n' > "$POLICY" ;;
        utf16le-no-bom) printf 'g\000l\000i\000m\000m\000e\000r\000d\000e\000n\000y\000\n\000' > "$POLICY" ;;
        utf16be-no-bom) printf '\000g\000l\000i\000m\000m\000e\000r\000d\000e\000n\000y\000\n' > "$POLICY" ;;
        nul) printf 'glimmerdeny\000\n' > "$POLICY" ;;
        nul-comment) printf '# comment\000\n\nglimmerdeny\n' > "$POLICY" ;;
        invalid-lead) printf 'glimmerdeny\377\n' > "$POLICY" ;;
        stray-continuation) printf 'glimmerdeny\200\n' > "$POLICY" ;;
        bad-continuation) printf 'glimmerdeny\303x\n' > "$POLICY" ;;
        truncated-two) printf 'glimmerdeny\302' > "$POLICY" ;;
        truncated-three) printf 'glimmerdeny\342\202' > "$POLICY" ;;
        truncated-four) printf 'glimmerdeny\360\237\222' > "$POLICY" ;;
        overlong-two) printf 'glimmerdeny\300\200\n' > "$POLICY" ;;
        overlong-three) printf 'glimmerdeny\340\200\200\n' > "$POLICY" ;;
        overlong-four) printf 'glimmerdeny\360\200\200\200\n' > "$POLICY" ;;
        surrogate) printf 'glimmerdeny\355\240\200\n' > "$POLICY" ;;
        above-unicode) printf 'glimmerdeny\364\220\200\200\n' > "$POLICY" ;;
        invalid-comment) printf '# caf\303\n\nglimmerdeny\n' > "$POLICY" ;;
    esac
    reject "unsupported policy encoding $encoding" 'must be valid UTF-8 without BOM or NUL' origin main
    not_ready "unsupported policy encoding $encoding" 'must be valid UTF-8 without BOM or NUL'
done
printf '#\tASCII comment\r\n\r\nglimmerdeny\r\notherdeny\r\n!glimmerdeny.example/ok' > "$POLICY"
reject 'ASCII policy denies intended entry' 'line 3' origin main
reset_case
printf 'ASCII control\n' > "$R/data"; commit
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --require-ready > "$T/setup.out" 2>&1 || fail 'ASCII policy readiness refused'
accept 'ASCII policy ready and clean push accepted' origin main

# Non-ASCII comments are ignored by matching, but must be valid UTF-8. Include
# the two/three/four-byte boundaries so validation does not reject legal scalars.
printf '# caf\303\251\t\302\200\337\277\340\240\200\355\237\277\356\200\200\357\277\277\360\220\200\200\364\217\277\277\r\n\r\nglimmerdeny\r\notherdeny\r\n!glimmerdeny.example/ok' > "$POLICY"
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --require-ready > "$T/setup.out" 2> "$T/ready.err" || fail 'UTF-8 comment policy readiness refused'
grep -F 'policy ready' "$T/setup.out" > /dev/null || fail 'UTF-8 comment policy did not report ready'
audit_stderr "$T/ready.err" 'UTF-8 comment policy readiness'
ok 'UTF-8 comment policy readiness'
printf 'UTF-8 comment control\n' > "$R/data"; commit
accept 'UTF-8 non-ASCII comment permits clean push' origin main
BASE=$(git -C "$R" rev-parse HEAD)
printf GLIMMERDENY > "$R/data"; commit
reject 'UTF-8 comment preserves ASCII denial and line number' 'line 3' origin main
reset_case

# Existing LC_ALL=C matching folds ASCII letters only; the accent bytes are
# literal. Neither Unicode case folding nor normalization is introduced.
printf '# caf\303\251\n\nglimmerdeny\303\251\n!glimmerdeny\303\251.example/ok\n' > "$POLICY"
"$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --require-ready > "$T/setup.out" 2>&1 || fail 'UTF-8 entry policy readiness refused'
ok 'UTF-8 entry policy readiness'
printf 'GLIMMERDENY\303\211\n' > "$R/data"; commit
accept 'non-ASCII entry does not Unicode case fold' origin main
printf 'GLIMMERDENYe\314\201\n' > "$R/data"; commit
accept 'non-ASCII entry does not normalize Unicode' origin main
BASE=$(git -C "$R" rev-parse HEAD)
printf 'GLIMMERDENY\303\251\n' > "$R/data"; commit
reject 'non-ASCII entry matches literal UTF-8 bytes with ASCII folding' 'line 3' origin main
reset_case
printf 'GLIMMERDENY\303\251.example/ok\n' > "$R/data"; commit
accept 'non-ASCII literal allowance retains matching behavior' origin main
printf '%s\n' '# synthetic policy' '' 'glimmerdeny' 'otherdeny' '!glimmerdeny.example/ok' > "$POLICY"

# Each inherited selector is hostile independently; the combined case includes
# config injection and discovery knobs. The named clone alone may be enrolled.
fixture selector-target
SELECTOR_TARGET=$R; SELECTOR_REMOTE=$B
fixture selector-decoy
DECOY=$R
# Leave the decoy's owned slot valid: a redirected setup could otherwise fail
# merely because of a foreign-hook conflict, hiding the wrong-clone defect.
cp "$DECOY/.git/hooks/pre-push" "$T/decoy-hook"
cp "$POLICY" "$T/decoy-publish-list.txt"
git -C "$DECOY" config --local sot.publishList "$T/decoy-publish-list.txt"
cp "$DECOY/.git/config" "$T/decoy-config"
git -C "$DECOY" config --file "$T/hostile-config" core.hooksPath "$T/hostile-hooks"
R=$SELECTOR_TARGET; B=$SELECTOR_REMOTE
printf glimmerdeny > "$R/data"; commit
for selector in GIT_DIR GIT_COMMON_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CONFIG combined; do
    rm -f "$R/.git/hooks/pre-push"
    git -C "$R" config --local --unset-all sot.publishList
    case "$selector" in
        GIT_DIR|GIT_COMMON_DIR) hostile="$DECOY/.git" ;;
        GIT_WORK_TREE) hostile=$DECOY ;;
        GIT_INDEX_FILE) hostile="$DECOY/.git/index" ;;
        GIT_OBJECT_DIRECTORY|GIT_ALTERNATE_OBJECT_DIRECTORIES) hostile="$DECOY/.git/objects" ;;
        GIT_CONFIG) hostile="$T/hostile-config" ;;
    esac
    if [ "$selector" = combined ]; then
        env GIT_DIR="$DECOY/.git" GIT_COMMON_DIR="$DECOY/.git" GIT_WORK_TREE="$DECOY" \
            GIT_INDEX_FILE="$DECOY/.git/index" GIT_OBJECT_DIRECTORY="$DECOY/.git/objects" \
            GIT_ALTERNATE_OBJECT_DIRECTORIES="$DECOY/.git/objects" GIT_CONFIG="$T/hostile-config" \
            GIT_NAMESPACE=decoy GIT_PREFIX=decoy/ GIT_SHALLOW_FILE="$T/absent-shallow" \
            GIT_CEILING_DIRECTORIES="$T" GIT_DISCOVERY_ACROSS_FILESYSTEM=0 \
            GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0="$T/hostile-hooks" \
            "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --list "$POLICY" --require-ready > "$T/setup.out" 2>&1 || fail "selector enrollment $selector refused"
    else
        env "$selector=$hostile" "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --list "$POLICY" --require-ready > "$T/setup.out" 2>&1 || fail "selector enrollment $selector refused"
    fi
    cmp "$T/decoy-hook" "$DECOY/.git/hooks/pre-push" || fail "selector $selector changed another clone's hook"
    cmp "$T/decoy-config" "$DECOY/.git/config" || fail "selector $selector changed another clone's config"
    # A real new-ref push demonstrates the named clone's hook is active.
    reject "hostile selector $selector binds named clone" 'line 3' origin HEAD:refs/heads/selector-control
    recorded=$(git -C "$R" config --local sot.publishList) || fail "selector $selector did not provision named clone"
    case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) recorded=$(cygpath -u "$recorded") ;; esac
    [ "$recorded" -ef "$POLICY" ] || fail "selector $selector did not provision named clone"
done
printf '1..%s\n' "$count"
