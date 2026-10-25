#!/bin/bash
# Real local pushes; every denial has a clean control and an unchanged remote.
. "${BASH_SOURCE[0]%/*}/publish-guard-fixture.sh"
# Select any one case alone (PUBLISH_GUARD_CASE=tracking-<name>) to witness it against another hook.
case "${PUBLISH_GUARD_CASE:-}" in ''|tracking-*)
    fixture tracking-history
    printf 'glimmerdeny\n' > "$R/data"; commit 'old snapshot'
    git -C "$R" push -q --no-verify origin main > "$T/seed.out" 2>&1
    printf 'clean again\n' > "$R/data"; commit 'remove old content'
    git -C "$R" push -q --no-verify origin main > "$T/seed.out" 2>&1
    R="$T/with spaces/fetched-clone"
    git clone -q --no-local -b main "$B" "$R"
    payload "$R"
    "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --list "$POLICY" > "$T/setup.out" 2>&1 || fail 'fetched clone enrollment failed'
    fetched=$(git -C "$R" rev-parse refs/remotes/origin/main)
    [ "$fetched" = "$(git -C "$B" rev-parse refs/heads/main)" ] || fail 'clone did not fetch destination history'
    commit 'new clean snapshot'
    git -C "$R" branch fetched-branch
    git -C "$R" tag -a fetched-tag -m 'new clean tag'
    branch_update=refs/heads/fetched-branch:refs/heads/fetched-branch
    selected=0
    for field in path url unrelated overlap replaced mirror bareword pushurl retained branch tag; do
        [ -z "${PUBLISH_GUARD_CASE:-}" ] || [ "$PUBLISH_GUARD_CASE" = "tracking-$field" ] || continue
        selected=$((selected + 1))
        case "$field" in
            path) accept 'new branch by path excludes advertised history' "$B" refs/heads/fetched-branch:refs/heads/by-path ;;
            url) accept 'new branch by URL excludes advertised history' "file://$B" refs/heads/fetched-branch:refs/heads/by-url ;;
            unrelated)
                destination=$B
                B="$T/with spaces/other-destination.git"
                git init -q --bare "$B"
                git -C "$R" remote add other "$B"
                reject 'a destination that does not advertise the history excludes none of it' 'line 3' other "$branch_update"
                B=$destination ;;
            overlap)
                # Remote `side` fetches into `pub`'s tracking namespace (refs/remotes/pub/side/*); only what `pub` advertises counts.
                destination=$B
                B="$T/with spaces/pub.git"; side="$T/with spaces/side.git"
                git init -q --bare "$B"; git init -q --bare "$side"
                git -C "$R" remote add pub "$B"; git -C "$R" remote add side "$side"
                git -C "$R" config remote.side.fetch '+refs/heads/*:refs/remotes/pub/side/*'
                git -C "$R" push -q --no-verify side refs/heads/main:refs/heads/main > "$T/seed.out" 2>&1
                git -C "$R" fetch -q side
                git -C "$R" rev-parse --verify -q refs/remotes/pub/side/main > /dev/null || fail 'overlap fixture lacks a nested tracking ref'
                reject 'a nested remote namespace excludes nothing the pushed destination does not advertise' 'line 3' pub "$branch_update"
                B=$destination ;;
            replaced)
                # The destination's tip was force-replaced; the stale tracking tip is no longer advertised.
                destination=$B
                B="$T/with spaces/replaced.git"
                git init -q --bare "$B"
                git -C "$R" remote add replaced "$B"
                git -C "$R" push -q --no-verify replaced refs/heads/fetched-branch:refs/heads/main > "$T/seed.out" 2>&1
                git -C "$R" fetch -q replaced
                # Replace the tip with the root commit, which holds none of the denied history.
                git -C "$B" update-ref refs/heads/main "$(git -C "$R" rev-list --max-parents=0 refs/heads/fetched-branch)"
                [ "$(git -C "$R" rev-parse refs/remotes/replaced/main)" = "$(git -C "$R" rev-parse refs/heads/fetched-branch)" ] || fail 'replaced fixture lacks a stale tracking tip'
                stale_child=$(printf 'clean descendant of the stale tip' | git -C "$R" commit-tree "$(git -C "$R" rev-parse 'refs/heads/fetched-branch^{tree}')" -p refs/heads/fetched-branch)
                reject 'a force-replaced destination excludes nothing it no longer advertises' 'line 3' replaced "$stale_child:refs/heads/stale-descendant"
                B=$destination ;;
            mirror)
                # Fetch through a mirror, push upstream: ls-remote would rewrite the URL again and read the mirror.
                destination=$B
                B="$T/with spaces/upstream.git"; mirror="$T/with spaces/mirror.git"
                git init -q --bare "$B"; git init -q --bare "$mirror"
                git -C "$R" push -q --no-verify "$B" refs/heads/fetched-branch:refs/heads/main > "$T/seed.out" 2>&1
                printf 'glimmerdeny\n' > "$R/data"; commit 'mirror only'
                git -C "$R" push -q --no-verify "$mirror" HEAD:refs/heads/private > "$T/seed.out" 2>&1
                printf 'clean again\n' > "$R/data"; commit 'clean on mirror only'
                git -C "$R" config "url.$mirror.insteadOf" "$B"
                git -C "$R" config "url.$B.pushInsteadOf" "$B"
                reject 'a mirror rewrite cannot supply the destination advertisement' 'would come from another URL' "$B" HEAD:refs/heads/mirror-child
                git -C "$R" config --unset-all "url.$mirror.insteadOf"
                git -C "$R" config --unset-all "url.$B.pushInsteadOf"
                git -C "$R" reset -q --hard refs/heads/fetched-branch
                B=$destination ;;
            bareword)
                # A URL that is a bare word equal to another configured remote's name must not read that remote.
                destination=$B
                B="$R/wordrepo"
                git init -q --bare "$B"
                holder="$T/with spaces/holder.git"
                git init -q --bare "$holder"
                git -C "$R" push -q --no-verify "$holder" refs/heads/fetched-branch:refs/heads/main > "$T/seed.out" 2>&1
                git -C "$R" remote add wordrepo "$holder"
                git -C "$R" remote add pubword wordrepo
                reject 'a bare-word URL cannot read the remote of that name' 'would come from another URL' pubword "$branch_update"
                git -C "$R" remote remove wordrepo
                git -C "$R" remote remove pubword
                rm -rf "${B:?}"
                B=$destination ;;
            pushurl)
                # The hook asks the URL git pushes to, not the one the remote fetches from.
                destination=$B
                B="$T/with spaces/pushurl-empty.git"
                git init -q --bare "$B"
                git -C "$R" remote add split "$destination"
                git -C "$R" remote set-url --push split "$B"
                reject 'a push URL that holds none of the history excludes none of it' 'line 3' split "$branch_update"
                B=$destination ;;
            retained)
                printf 'glimmerdeny\n' > "$R/data"; commit 'new denied snapshot'
                reject 'new snapshot on fetched history still scans retained content' 'line 3' origin HEAD:refs/heads/new-denied
                git -C "$R" reset -q --hard refs/heads/fetched-branch ;;
            branch) accept 'new branch excludes fetched destination history' origin refs/heads/fetched-branch:refs/heads/by-name ;;
            tag) accept 'new tag excludes fetched destination history' origin refs/tags/fetched-tag ;;
        esac
    done
    if [ -n "${PUBLISH_GUARD_CASE:-}" ]; then
        [ "$selected" = 1 ] || fail 'unknown selected tracking regression'
        printf '1..%s\n' "$count"; exit 0
    fi
    ;;
esac
fixture scanner
ok 'clean commit push'
selected=0
for field in message text line-start line-end in-path host-label after-at hyphenated punctuation underscore non-ascii-boundaries later-word binary-before binary-after buffer-boundary path rename author-name author-email committer-name committer-email symlink odd-path; do
    [ -z "${PUBLISH_GUARD_CASE:-}" ] || [ "$PUBLISH_GUARD_CASE" = "$field" ] || continue
    selected=$((selected + 1))
    reset_case
    case "$field" in
        message) commit 'a GLIMMERDENY message' ;;
        text) printf 'GLIMMERDENY\n' > "$R/data"; commit ;;
        line-start) printf 'GLIMMERDENY begins here\n' > "$R/data"; commit ;;
        line-end) printf 'ends here GLIMMERDENY\n' > "$R/data"; commit ;;
        in-path) printf '/srv/GLIMMERDENY/x\n' > "$R/data"; commit ;;
        host-label) printf 'GLIMMERDENY.example.org\n' > "$R/data"; commit ;;
        after-at) printf 'user@GLIMMERDENY\n' > "$R/data"; commit ;;
        hyphenated) printf 'probe-GLIMMERDENY\n' > "$R/data"; commit ;;
        punctuation) printf '([GLIMMERDENY]),;\n' > "$R/data"; commit ;;
        underscore) printf '_GLIMMERDENY_\n' > "$R/data"; commit ;;
        non-ascii-boundaries) printf '\303\251GLIMMERDENY\303\251\n' > "$R/data"; commit ;;
        later-word) printf 'xglimmerdeny glimmerdenying [GLIMMERDENY]\n' > "$R/data"; commit ;;
        binary-before) printf 'glimmerdeny\000tail\n' > "$R/data"; commit ;;
        binary-after) printf 'head\000glimmerdeny\n' > "$R/data"; commit ;;
        buffer-boundary)
            # Keep the entry at the same byte offset, with a whole-word boundary after the padding.
            { printf '%08189d ' 0; printf 'glimmerdeny\n'; } > "$R/data"; commit ;;
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
# These acceptance regressions are red under substring matching. Distinct destinations
# keep each push observable without advancing the clean control used by later denials.
for field in word-prefix word-suffix word-both digit-prefix digit-suffix path branch identity message; do
    reset_case
    destination="refs/heads/longer-$field"
    case "$field" in
        word-prefix) printf 'xGLIMMERDENY\n' > "$R/data"; commit ;;
        word-suffix) printf 'GLIMMERDENYing\n' > "$R/data"; commit ;;
        word-both) printf 'xGLIMMERDENYing\n' > "$R/data"; commit ;;
        digit-prefix) printf '7GLIMMERDENY\n' > "$R/data"; commit ;;
        digit-suffix) printf 'GLIMMERDENY7\n' > "$R/data"; commit ;;
        path) mkdir -p "$R/glimmerdenying"; printf fine > "$R/glimmerdenying/x"; commit ;;
        branch) commit; destination=refs/heads/glimmerdenying ;;
        identity)
            GIT_AUTHOR_NAME=xglimmerdeny GIT_AUTHOR_EMAIL=glimmerdenying@example.invalid \
                GIT_COMMITTER_NAME=glimmerdenying GIT_COMMITTER_EMAIL=xglimmerdeny@example.invalid \
                git -C "$R" commit -q --allow-empty -m clean ;;
        message) commit 'xglimmerdeny glimmerdenying' ;;
    esac
    accept "longer word $field" origin "HEAD:$destination"
done
# Dots are literal entry bytes; the same ASCII boundary rule applies at both ends.
printf '%s\n' '# synthetic dotted policy' '' 'glimmerdeny.example.org' > "$POLICY"
for field in alone punctuation later-word; do
    reset_case
    case "$field" in
        alone) printf 'GLIMMERDENY.EXAMPLE.ORG\n' > "$R/data" ;;
        punctuation) printf '(GlImMeRdEnY.ExAmPlE.OrG):443\n' > "$R/data" ;;
        later-word) printf 'xglimmerdeny.example.org glimmerdeny.example.orging [GLIMMERDENY.EXAMPLE.ORG]\n' > "$R/data" ;;
    esac
    commit
    reject "dotted entry $field" 'line 3' origin main
done
for field in prefix suffix literal-dots; do
    reset_case
    case "$field" in
        prefix) printf 'xglimmerdeny.example.org\n' > "$R/data" ;;
        suffix) printf 'glimmerdeny.example.orging\n' > "$R/data" ;;
        literal-dots) printf 'glimmerdenyXexampleYorg\n' > "$R/data" ;;
    esac
    commit
    accept "dotted entry $field" origin "HEAD:refs/heads/dotted-$field"
done
printf 'glimmerdeny.glimmerdeny\n' > "$POLICY"
reset_case
printf 'xglimmerdeny.glimmerdeny.glimmerdeny\n' > "$R/data"; commit
reject 'overlapping dotted entry after an embedded occurrence' 'line 1' origin main
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
# the destination's advertised old commit and its ancestry may be excluded.
reset_case
printf 'clean again\n' > "$R/data"; commit
[ "$(git -C "$R" rev-parse HEAD^)" = "$old" ] || fail 'clean update does not directly follow the published commit'
[ "$(git -C "$B" rev-parse refs/heads/old)" = "$old" ] || fail 'destination no longer advertises the published commit'
accept 'advertised old history excluded after content removal' origin HEAD:old
# A new ref on the same destination excludes history the destination now advertises.
accept 'new ref excludes history the destination advertises' origin HEAD:refs/heads/new-history
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
# A destination that advertises nothing excludes nothing, so the shallow tip's missing parent is reached.
E="$T/with spaces/shallow-empty.git"
git init -q --bare "$E"
reject 'shallow missing parent' 'unavailable locally' "$E" HEAD:refs/heads/shallow-copy
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
    ls-remote:resolve) [ "${2:-}" != --get-url ] || fault ;;
    ls-remote:advertise) [ "${2:-}" = --get-url ] || fault ;;
esac
exec "$PG_REAL_GIT" "$@"
WRAPPER
fixture_executable "$T/git-wrapper/git"
# Git prepends its execution directory to PATH before running hooks. Put the fixture
# wrapper there, and retain the real execution directory on PATH for transport helpers.
for producer in catalog read tree resolve advertise; do
    : > "$T/producer-fault"
    case "$producer" in
        catalog) reason='pre-push: cannot enumerate local objects' ;;
        read) reason='pre-push: cannot read object' ;;
        tree) reason='pre-push: cannot read tree' ;;
        resolve) reason='pre-push: cannot resolve destination' ;;
        advertise) reason='pre-push: cannot read destination advertisement' ;;
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
