#!/bin/bash
# Real local pushes; every denial has a clean control and an unchanged remote.
. "${BASH_SOURCE[0]%/*}/publish-guard-fixture.sh"
fixture scanner
ok 'clean commit push'
selected=0
for field in message text binary-before binary-after buffer-boundary path rename author-name author-email committer-name committer-email symlink odd-path; do
    [ -z "${PUBLISH_GUARD_CASE:-}" ] || [ "$PUBLISH_GUARD_CASE" = "$field" ] || continue
    selected=$((selected + 1))
    reset_case
    case "$field" in
        message) commit 'a GLIMMERDENY message' ;;
        text) printf 'GLIMMERDENY\n' > "$R/data"; commit ;;
        binary-before) printf 'glimmerdeny\000tail\n' > "$R/data"; commit ;;
        binary-after) printf 'head\000glimmerdeny\n' > "$R/data"; commit ;;
        buffer-boundary) { printf '%08190d' 0; printf 'glimmerdeny\n'; } > "$R/data"; commit ;;
        path) mkdir -p "$R/parent"; printf 'fine\n' > "$R/parent/glimmerdeny file"; commit ;;
        odd-path)
            blob=$(printf fine | git -C "$R" hash-object -w --stdin)
            odd_tree=$(printf '100644 blob %s\ttab\tnewline\nglimmerdeny[?]\000' "$blob" | git -C "$R" mktree -z)
            odd_commit=$(printf clean | git -C "$R" commit-tree "$odd_tree" -p "$BASE")
            git -C "$R" update-ref HEAD "$odd_commit" ;;
        rename) git -C "$R" mv data glimmerdeny; commit ;;
        author-name) GIT_AUTHOR_NAME=glimmerdeny git -C "$R" commit -q --allow-empty -m clean ;;
        author-email) GIT_AUTHOR_EMAIL=glimmerdeny@example.invalid git -C "$R" commit -q --allow-empty -m clean ;;
        committer-name) GIT_COMMITTER_NAME=glimmerdeny git -C "$R" commit -q --allow-empty -m clean ;;
        committer-email) GIT_COMMITTER_EMAIL=glimmerdeny@example.invalid git -C "$R" commit -q --allow-empty -m clean ;;
        symlink)
            # Build a symlink object without needing Windows symlink privileges.
            blob=$(printf 'glimmerdeny' | git -C "$R" hash-object -w --stdin)
            git -C "$R" update-index --add --cacheinfo "120000,$blob,link"
            git -C "$R" commit -qm clean ;;
    esac
    reject "$field" 'line 3' origin main
    grep -E 'pre-push: (commit|blob|tree) [0-9a-f]+ holds' "$T/diagnostic" > /dev/null || fail 'content diagnostic lacks object identity'
done
# A selected prior-scanner regression ends here; its clean setup push was the prerequisite control.
if [ -n "${PUBLISH_GUARD_CASE:-}" ]; then
    [ "$selected" = 1 ] || fail 'unknown selected regression'
    printf '1..%s\n' "$count"; exit 0
fi
reset_case
# A literal slash-spanning policy is checked against full paths, not individual tree components.
printf 'parent/leaf\n' > "$POLICY"
mkdir -p "$R/parent"; printf fine > "$R/parent/leaf"; commit
reject 'full nested path' 'line 1' origin main
printf '%s\n' '# synthetic policy' '' 'glimmerdeny' 'otherdeny' '!glimmerdeny.example/ok' > "$POLICY"
reset_case
printf 'GlImMeRdEnY.example/ok\n' > "$R/data"; commit
accept 'literal case-insensitive allowed span' origin main
BASE=$(git -C "$R" rev-parse HEAD)
printf 'glimmerdeny.example/ok and otherdeny\n' > "$R/data"; commit
reject 'allowance leaves a second entry' 'line 4' origin main
reset_case
for destination in refs/heads/glimmerdeny refs/tags/glimmerdeny; do
    reject 'destination refspec name' 'update 1 (object' origin "HEAD:$destination"
done
git -C "$R" tag -a glimmerdeny -m clean
reject 'annotated destination tag name' 'update 1 (object' origin refs/tags/glimmerdeny
git -C "$R" tag -a tag-message -m otherdeny
reject 'annotated tag message' 'line 4' origin tag-message
for identity in name email; do
    if [ "$identity" = name ]; then
        GIT_COMMITTER_NAME=glimmerdeny git -C "$R" tag -a "tagger-$identity" -m clean
    else
        GIT_COMMITTER_EMAIL=glimmerdeny@example.invalid git -C "$R" tag -a "tagger-$identity" -m clean
    fi
    reject "tagger $identity" 'line 3' origin "tagger-$identity"
done
# Push exact OIDs through a clean destination, including nested tags and noncommit targets.
tag=$(printf 'object %s\ntype commit\ntag glimmerdeny\ntagger Fixture <fixture@example.invalid> 1 +0000\n\nclean\n' "$BASE" | git -C "$R" mktag)
reject 'embedded tag name by OID' 'line 3' origin "$tag:refs/tags/clean-name"
nested=$(printf 'object %s\ntype tag\ntag nested\ntagger Fixture <fixture@example.invalid> 1 +0000\n\nclean\n' "$tag" | git -C "$R" mktag)
reject 'nested tag exact target' 'line 3' origin "$nested:refs/tags/nested"
blob=$(printf 'glimmerdeny\000payload' | git -C "$R" hash-object -w --stdin)
tag=$(printf 'object %s\ntype blob\ntag blob-tag\ntagger Fixture <fixture@example.invalid> 1 +0000\n\nclean\n' "$blob" | git -C "$R" mktag)
reject 'annotated blob tag' 'line 3' origin "$tag:refs/tags/blob-tag"
reject 'lightweight blob tag' 'line 3' origin "$blob:refs/tags/raw-blob"
tree=$(printf '100644 blob %s\tclean\n' "$blob" | git -C "$R" mktree)
reject 'tree tag' 'line 3' origin "$tree:refs/tags/tree-tag"
# All bytes in retained files and merge snapshots are in scope.
printf 'glimmerdeny\n' > "$R/data"; commit
old=$(git -C "$R" rev-parse HEAD)
git -C "$R" push -q --no-verify origin HEAD:old > "$T/push.out" 2>&1
BASE=$old
commit 'new snapshot'
reject 'retained denied content' 'line 3' origin HEAD:old
printf 'clean again\n' > "$R/data"; commit
reject 'clean tip still scans unpublished denied parent' 'line 3' origin HEAD:old
# The rejected snapshot was never published. Leave it off the clean update's ancestry;
# only the destination's advertised old commit and its ancestors may be excluded.
reset_case
printf 'clean again\n' > "$R/data"; commit
[ "$(git -C "$R" rev-parse HEAD^)" = "$old" ] || fail 'clean update does not directly follow the published commit'
[ "$(git -C "$B" rev-parse refs/heads/old)" = "$old" ] || fail 'destination no longer advertises the published commit'
accept 'advertised old history excluded after content removal' origin HEAD:old
# Another remote-tracking namespace is not an exclusion.
git -C "$R" update-ref refs/remotes/unrelated/held "$old"
reject 'unrelated remote history' 'line 3' origin "$old:refs/heads/another"
# New refs conservatively recheck ancestors even if a different destination ref already holds them.
reject 'new ref rechecks history' 'line 3' origin HEAD:refs/heads/new-history
merged=$(printf clean | git -C "$R" commit-tree "$tree" -p HEAD -p "$BASE")
reject 'merge tree content' 'line 3' origin "$merged:refs/heads/merge-content"
# A gitlink contributes its path and oid without fetching its repository.
git -C "$R" reset -q --hard HEAD
subtree=$(printf '160000 commit %s\tsubmodule\n' "$old" | git -C "$R" mktree)
subcommit=$(printf clean | git -C "$R" commit-tree "$subtree")
accept 'gitlink needs no submodule checkout' origin "$subcommit:refs/heads/submodule"
# Missing local objects must refuse without attempting a promisor fetch.
fixture object-controls
cat > "$T/diff-probe" <<'DIFF'
#!/bin/sh
printf invoked > "$HOME/diff-called"
exit 0
DIFF
fixture_executable "$T/diff-probe"
git -C "$R" config diff.external "$T/diff-probe"
git -C "$R" config diff.fixture.textconv "$T/diff-probe"
printf 'data diff=fixture\n' > "$R/.gitattributes"
printf clean2 > "$R/data"
# Both native Git helper routes must really work before absence can prove anything.
git -C "$R" diff --ext-diff HEAD -- data > "$T/diff-control.out" 2>&1 || fail 'external diff execution control failed'
[ -s "$HOME/diff-called" ] || fail 'external diff probe did not run'
rm -f "$HOME/diff-called"
git -C "$R" diff --no-ext-diff --textconv HEAD -- data > "$T/diff-control.out" 2>&1 || fail 'textconv execution control failed'
[ -s "$HOME/diff-called" ] || fail 'textconv probe did not run'
rm -f "$HOME/diff-called"
commit
accept 'raw object read ignores diff/textconv configuration' origin main
[ ! -e "$HOME/diff-called" ] || fail 'object scan executed a diff helper'
printf glimmerdeny > "$R/data"; commit
bad=$(git -C "$R" rev-parse HEAD)
clean=$(printf clean | git -C "$R" commit-tree "$(git -C "$R" rev-parse 'origin/main^{tree}')" -p origin/main)
git -C "$R" replace "$bad" "$clean"
reject 'replacement object cannot hide original bytes' 'line 3' origin main
[ ! -e "$HOME/diff-called" ] || fail 'denial executed a diff helper'
# An advertised old oid unavailable locally is not a trusted remote-tracking shortcut.
fixture unavailable-old
O="$T/remote-writer"
git clone -q -b main "$B" "$O"
printf remote > "$O/data"
git -C "$O" add data; git -C "$O" commit -qm 'remote only'
git -C "$O" push -q origin main
printf glimmerdeny > "$R/data"; commit
reject 'unavailable advertised old oid scans conservatively' 'line 3' --force origin main
fixture promisor
printf 'locally missing\n' > "$R/data"; commit
missing=$(git -C "$R" rev-parse HEAD:data)
mv "$R/.git/objects/${missing:0:2}/${missing:2}" "$T/held-object"
git -C "$R" config extensions.partialClone origin
git -C "$R" config remote.origin.promisor true
git -C "$R" config remote.origin.partialclonefilter blob:none
# Any implicit fetch would run this recording upload-pack; the ordinary push uses receive-pack.
cat > "$T/upload-pack" <<'STUB'
#!/bin/sh
printf 'fetch\n' >> "$HOME/fetch-called"
exit 1
STUB
fixture_executable "$T/upload-pack"
git -C "$R" config remote.origin.uploadpack "$T/upload-pack"
if git -C "$R" ls-remote --upload-pack="$T/upload-pack" origin > "$T/upload-control.out" 2>&1; then fail 'upload-pack execution control accepted'; fi
[ -s "$HOME/fetch-called" ] || fail 'upload-pack probe did not run'
rm -f "$HOME/fetch-called"
reject 'missing promisor blob' 'unavailable locally' origin main
[ ! -e "$HOME/fetch-called" ] || fail 'scanner attempted a lazy fetch'
ok 'no lazy fetch'
mv "$T/held-object" "$R/.git/objects/${missing:0:2}/${missing:2}"
accept 'restored local object' origin main
# A shallow clone must not silently skip missing raw parent objects.
S="$T/shallow"
git clone -q --branch main --depth=1 "file://$B" "$S"
payload "$S"
"$BASH" "$S/scripts/setup-publish-guard.sh" "$S" --list "$POLICY" > "$T/setup.out"
R=$S
reject 'shallow missing parent' 'unavailable locally' origin HEAD:refs/heads/shallow-copy
fixture producers
printf new > "$R/data"; commit
REAL_GIT=$(command -v git)
REAL_GIT_EXEC_PATH=$("$REAL_GIT" --exec-path)
FAULT_EXEC_PATH="$T/git-wrapper"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*)
    REAL_GIT_EXEC_PATH=$(cygpath -u "$REAL_GIT_EXEC_PATH")
    FAULT_EXEC_PATH=$(cygpath -am "$FAULT_EXEC_PATH")
    ;; esac
mkdir "$T/git-wrapper"
cat > "$T/git-wrapper/git" <<'WRAPPER'
#!/bin/bash
fault() {
    printf '%s\n' "$PG_FAIL_PRODUCER" >> "$PG_FAULT_LOG" || exit 98
    exit 9
}
case "$1:${PG_FAIL_PRODUCER:-}" in
    cat-file:catalog) [ "${2:-}" != --batch-all-objects ] || fault ;;
    cat-file:read) [ "${2:-}" != commit ] || fault ;;
    ls-tree:tree) fault ;;
esac
exec "$PG_REAL_GIT" "$@"
WRAPPER
fixture_executable "$T/git-wrapper/git"
# Git prepends its execution directory to PATH before running hooks. Put the fixture
# wrapper there, and retain the real execution directory on PATH for transport helpers.
for producer in catalog read tree; do
    : > "$T/producer-fault"
    case "$producer" in
        catalog) reason='pre-push: cannot enumerate local objects' ;;
        read) reason='pre-push: cannot read object' ;;
        tree) reason='pre-push: cannot read tree' ;;
    esac
    PG_FAIL_PRODUCER=$producer PG_REAL_GIT=$REAL_GIT PG_FAULT_LOG="$T/producer-fault" \
        GIT_EXEC_PATH="$FAULT_EXEC_PATH" PATH="$T/git-wrapper:$REAL_GIT_EXEC_PATH:$PATH" \
        reject "failed Git producer $producer" "$reason" origin main
    grep -Fx "$producer" "$T/producer-fault" > /dev/null || fail 'wrong Git producer fault was injected'
done
# The denied token is deliberately in this synthetic pathname: any utility error
# repeating it must be caught by the same complete-stderr audit as a content leak.
REAL_CAT=$(command -v cat)
FAULT_POLICY="$T/glimmerdeny-policy.txt"
cp "$POLICY" "$FAULT_POLICY"
git -C "$R" config sot.publishList "$FAULT_POLICY"
cat > "$T/git-wrapper/cat" <<'CAT'
#!/bin/bash
policy=$PG_POLICY_FILE
real_cat=$PG_REAL_CAT
fault_log=$PG_FAULT_LOG
held=$PG_HELD_POLICY
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*)
    # Git converts inherited paths to native spellings. The guard's argv uses
    # cygpath -u; compare file identities after normalizing both path forms.
    policy=$(cygpath -u "$policy") || exit 98
    real_cat=$(cygpath -u "$real_cat") || exit 98
    fault_log=$(cygpath -u "$fault_log") || exit 98
    held=$(cygpath -u "$held") || exit 98
    ;; esac
if [ -n "${1:-}" ] && [ "$1" -ef "$policy" ]; then
    printf 'policy-read\n' >> "$fault_log" || exit 98
    if [ "$PG_POLICY_ACTION" = vanished ]; then
        mv "$policy" "$held" || exit 98
        exec "$real_cat" "$@" # a real ENOENT diagnostic, including the path
    fi
    printf 'cat: %s: injected read error\n' "$1" >&2
    exit 9
fi
exec "$real_cat" "$@"
CAT
fixture_executable "$T/git-wrapper/cat"
for action in read-error vanished; do
    : > "$T/producer-fault"
    PG_POLICY_FILE=$FAULT_POLICY PG_REAL_CAT=$REAL_CAT PG_REAL_GIT=$REAL_GIT PG_FAULT_LOG="$T/producer-fault" \
        PG_POLICY_ACTION=$action PG_HELD_POLICY="$T/policy-held" \
        GIT_EXEC_PATH="$FAULT_EXEC_PATH" PATH="$T/git-wrapper:$REAL_GIT_EXEC_PATH:$PATH" \
        reject "failed policy read $action" 'cannot read sot.publishList' origin main
    grep -Fx policy-read "$T/producer-fault" > /dev/null || fail 'policy read fault was not injected'
    [ "$action" != vanished ] || mv "$T/policy-held" "$FAULT_POLICY"
    : > "$T/producer-fault"
    PG_POLICY_FILE=$FAULT_POLICY PG_REAL_CAT=$REAL_CAT PG_REAL_GIT=$REAL_GIT PG_FAULT_LOG="$T/producer-fault" \
        PG_POLICY_ACTION=$action PG_HELD_POLICY="$T/policy-held" \
        GIT_EXEC_PATH="$FAULT_EXEC_PATH" PATH="$T/git-wrapper:$REAL_GIT_EXEC_PATH:$PATH" \
        not_ready "failed policy read $action" 'cannot read sot.publishList'
    grep -Fx policy-read "$T/producer-fault" > /dev/null || fail 'readiness read fault was not injected'
    [ "$action" != vanished ] || mv "$T/policy-held" "$FAULT_POLICY"
done
git -C "$R" config sot.publishList "$POLICY"
rm -f "$T/git-wrapper/cat"
# Encoding validation has a checked producer too, on both readiness and push.
for encoding_producer in od awk; do
    cat > "$T/git-wrapper/$encoding_producer" <<'ENCODING'
#!/bin/bash
fault_log=$PG_FAULT_LOG
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) fault_log=$(cygpath -u "$fault_log") || exit 98 ;; esac
printf 'policy-encoding\n' >> "$fault_log" || exit 98
printf 'fixture encoding error: glimmerdeny-path\n' >&2
exit 9
ENCODING
    fixture_executable "$T/git-wrapper/$encoding_producer"
    : > "$T/producer-fault"
    PG_REAL_GIT=$REAL_GIT PG_FAULT_LOG="$T/producer-fault" \
        GIT_EXEC_PATH="$FAULT_EXEC_PATH" PATH="$T/git-wrapper:$REAL_GIT_EXEC_PATH:$PATH" \
        reject "failed encoding producer $encoding_producer" 'cannot validate sot.publishList encoding' origin main
    grep -Fx policy-encoding "$T/producer-fault" > /dev/null || fail 'encoding producer fault was not injected'
    : > "$T/producer-fault"
    PG_REAL_GIT=$REAL_GIT PG_FAULT_LOG="$T/producer-fault" \
        GIT_EXEC_PATH="$FAULT_EXEC_PATH" PATH="$T/git-wrapper:$REAL_GIT_EXEC_PATH:$PATH" \
        not_ready "failed encoding producer $encoding_producer" 'cannot validate sot.publishList encoding'
    grep -Fx policy-encoding "$T/producer-fault" > /dev/null || fail 'readiness encoding fault was not injected'
    rm -f "$T/git-wrapper/$encoding_producer"
done
# Prove the audit itself catches unprefixed stderr from a real refused push.
cat > "$R/.git/hooks/pre-push.local" <<'LEAK'
#!/bin/sh
cat >/dev/null
printf 'fixture utility: glimmerdeny-path: read error\n' >&2
exit 17
LEAK
fixture_executable "$R/.git/hooks/pre-push.local"
audit_status=0
(reject 'unprefixed stderr witness' 'fixture utility:' origin main) > "$T/audit.out" 2>&1 || audit_status=$?
[ "$audit_status" -ne 0 ] || fail 'stderr audit ignored an unprefixed leak'
grep -F 'unprefixed stderr witness leaked a policy entry' "$T/audit.out" > /dev/null || fail 'stderr audit failed for another reason'
ok 'complete stderr audit catches unprefixed leak'
rm -f "$R/.git/hooks/pre-push.local"
accept 'producer controls remain clean' origin main
accept 'deletion preparation' origin HEAD:old
accept 'valid-list deletion' origin :old
mv "$POLICY" "$T/policy-held"
reject 'missing list rejects deletion too' 'sot.publishList' origin :main
mv "$T/policy-held" "$POLICY"
GIT_DEFAULT_HASH=sha256 fixture object-format
accept 'SHA-256 deletion uses its advertised zero-id width' origin :main
printf '1..%s\n' "$count"
