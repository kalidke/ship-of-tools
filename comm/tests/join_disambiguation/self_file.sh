# test-join-disambiguation.sh part: legacy, v2, ancient and nopane self-files, and self-heal (sourced in order by the entry).

case_legacy_matching_self_file_is_reclaimed_not_rederived() {
    # Field regression fix (coordinator ruling, item 1). This case used to
    # assert the OPPOSITE of what follows: that a legacy (pre-#148, no
    # root=) self-file whose repo= line matches this project must be
    # discarded and forced through fresh derivation. That was exactly the
    # bug: PR #148 shipped that as unconditional, and EVERY self-file
    # written before it (the overwhelming majority in the field) lacks
    # root= — evicting every long-running session from its own identity on
    # its next comm call. A legacy self-file whose repo= matches must
    # instead be ACCEPTED and reclaimed verbatim (never re-derived), with
    # root= self-healed onto it so every subsequent read is fully
    # validated. See case_v2_self_file_wrong_root_is_still_discarded below
    # for the strict root= check this does NOT relax.
    #
    # Codex review round-1 finding 2: this case originally proved nothing
    # about OWNERSHIP — it seeded no registry row at all for
    # 'legacy-claimed-name', so the heal below exercised only the weaker
    # "no corroborating evidence, trust the basename" branch, which
    # codifies excessive trust rather than proving the fix. Seeding a
    # registry row with a MATCHING root here routes this through the
    # strongest branch instead — the heal is now corroborated by
    # independent registry evidence, not basename alone. See
    # case_legacy_selffile_registry_root_disagreement_refuses_heal below
    # for the mirror-image case (a DISAGREEING registry root, which must
    # refuse to heal).
    local root base h1 crafted seed_obj
    mkdir -p "$WORK/selftest/proj2"
    root="$(realpath "$WORK/selftest/proj2")"
    base="proj2"
    h1="${base}-${HOST}"

    seed_obj="$(jq -n --arg repo "$base" --arg root "$root" \
        '{host:"h",tmux:"",pane_id:"",repo:$repo,root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    with_lock registry_put "legacy-claimed-name" "$seed_obj"

    crafted="$WORK/crafted-self.txt"
    # A LEGACY two-line self-file (repo= but no root=). The point of this
    # test is that comm-join.sh must NOT re-derive at all — it must reclaim
    # 'legacy-claimed-name' verbatim, not land on the freshly-derived $h1.
    printf 'legacy-claimed-name\nrepo=%s\n' "$base" > "$crafted"

    JOIN_SELF_FILE_OVERRIDE="$crafted"
    join_in "$root"
    JOIN_SELF_FILE_OVERRIDE=""

    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "stale" && { echo "  unexpected staleness notice for a matching legacy self-file: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "self-healed" || { echo "  missing self-heal notice: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @legacy-claimed-name" \
        || { echo "  stdout: $JOIN_OUT (want the pre-existing identity 'legacy-claimed-name' reclaimed, NOT fresh derivation to @$h1)"; return 1; }
    [ "$(registry_root "legacy-claimed-name")" = "$root" ] \
        || { echo "  root=$(registry_root "legacy-claimed-name"), want $root"; return 1; }
    [ "$(registry_root "$h1")" = "MISSING" ] \
        || { echo "  fresh derivation ALSO ran and claimed @$h1: $(registry_root "$h1")"; return 1; }

    # The self-file must now have been rewritten to full v2 (root= present).
    local lines; lines="$(wc -l < "$crafted")"
    [ "$lines" -ge 3 ] || { echo "  self-file not upgraded to v2 with root=: $(cat "$crafted")"; return 1; }
    local backfilled_root; backfilled_root="$(sed -n '3p' "$crafted" | sed -n 's/^root=//p')"
    [ "$backfilled_root" = "$root" ] || { echo "  backfilled root=$backfilled_root, want $root"; return 1; }
    return 0
}

case_legacy_self_file_matching_repo_accepted_and_backfilled() {
    # (a) Direct comm-context.sh unit coverage of the same fix, with no
    # registry side effects: a legacy self-file (no root=) whose repo=
    # matches is accepted verbatim and self-healed on read, and does NOT
    # re-heal (or re-notify) on a second read once it's already v2.
    local root base self
    mkdir -p "$WORK/legacy-accept/proj4"
    root="$(realpath "$WORK/legacy-accept/proj4")"
    base="proj4"
    self="$WORK/legacy-accept-self.txt"
    printf 'my-own-handle\nrepo=%s\n' "$base" > "$self"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  comm-context.sh exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" && { echo "  unexpected staleness notice for a matching legacy self-file: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" || { echo "  missing self-heal notice: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "my-own-handle" ] || { echo "  NAME=$CTX_NAME, want my-own-handle (accepted, not re-derived)"; return 1; }

    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -ge 3 ] || { echo "  self-file not backfilled to v2 with root=: $(cat "$self")"; return 1; }
    local backfilled_root; backfilled_root="$(sed -n '3p' "$self" | sed -n 's/^root=//p')"
    [ "$backfilled_root" = "$root" ] || { echo "  backfilled root=$backfilled_root, want $root"; return 1; }

    # Second read: already v2 — no re-heal, no re-notify, same identity.
    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  second read exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  self-heal fired AGAIN on an already-v2 file: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "my-own-handle" ] || { echo "  second read NAME=$CTX_NAME, want my-own-handle"; return 1; }
    return 0
}

case_legacy_self_file_mismatched_repo_still_discarded() {
    # (b) The pane-recycling protection root= replaced must still hold: a
    # legacy self-file whose repo= does NOT match is discarded as stale,
    # exactly as before this fix — self-healing must never launder a real
    # mismatch into acceptance.
    local root self
    mkdir -p "$WORK/legacy-mismatch/proj5"
    root="$(realpath "$WORK/legacy-mismatch/proj5")"
    self="$WORK/legacy-mismatch-self.txt"
    printf 'someone-elses-handle\nrepo=totally-different-repo\n' > "$self"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (discarded)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  mismatched legacy self-file was mutated (must be left untouched): $(cat "$self")"; return 1; }
    return 0
}

case_v2_self_file_wrong_root_is_still_discarded() {
    # (c) root= present-and-wrong stays strict and unconditional — this
    # fix only widens acceptance for files that PREDATE root=, never for
    # ones that carry it and disagree.
    local rootA rootB self
    mkdir -p "$WORK/v2-wrong-root/proj6a" "$WORK/v2-wrong-root/proj6b"
    rootA="$(realpath "$WORK/v2-wrong-root/proj6a")"
    rootB="$(realpath "$WORK/v2-wrong-root/proj6b")"
    self="$WORK/v2-wrong-root-self.txt"
    printf 'handle-for-a\nrepo=proj6a\nroot=%s\n' "$rootA" > "$self"

    context_in "$rootB" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (discarded — root= present and wrong)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 3 ] || { echo "  self-file with a wrong root= was mutated (must be left untouched): $(cat "$self")"; return 1; }
    return 0
}

case_v2_self_file_empty_root_is_discarded_not_treated_as_absent() {
    # (d) Codex review round-1 finding 2: "distinguish an absent root= line
    # from a present-but-empty/malformed one" — a `root=` line that IS
    # present but carries no value must be discarded exactly like a wrong
    # one, never treated as if the line were missing altogether (which
    # would route it into the more permissive legacy-heal path below,
    # trusting a basename alone for a file that is supposed to already
    # carry root= evidence).
    local root self
    mkdir -p "$WORK/v2-empty-root/proj6c"
    root="$(realpath "$WORK/v2-empty-root/proj6c")"
    self="$WORK/v2-empty-root-self.txt"
    printf 'handle-for-c\nrepo=proj6c\nroot=\n' > "$self"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice for an empty root=: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (discarded — root= present but empty)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 3 ] || { echo "  self-file with an empty root= was mutated (must be left untouched): $(cat "$self")"; return 1; }
    return 0
}

case_malformed_third_line_discarded_not_treated_as_absent() {
    # Codex review round-3 finding 2: a present-but-malformed third line
    # (e.g. "rootBROKEN" — no root= prefix at all) used to be classified
    # as an ABSENT third line and routed through the more permissive
    # legacy-heal path (basename-alone trust). Array length (not just
    # pattern match) now tells "no third line" apart from "a garbage
    # third line" — the latter is corrupted evidence and discarded
    # unconditionally, same as a present-and-wrong root=.
    local root self
    mkdir -p "$WORK/malformed-root/proj19"
    root="$(realpath "$WORK/malformed-root/proj19")"
    self="$WORK/malformed-root-self.txt"
    printf 'proj19-handle\nrepo=proj19\nrootBROKEN\n' > "$self"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "malformed third line" || { echo "  missing the malformed-third-line refusal: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  SELF-HEALED a malformed third line: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 3 ] || { echo "  self-file with a malformed third line was mutated (must be left untouched): $(cat "$self")"; return 1; }
    return 0
}

case_registry_read_error_refuses_to_heal_or_write() {
    # Codex review round-3 finding 1: an unreadable/malformed registry.json
    # used to make sot_registry_entry_status print NOTHING, which every
    # caller read back as indistinguishable from "no row" — letting a
    # pane-keyed legacy self-file self-heal on a basename match with the
    # registry effectively unconsultable. A distinct "error" tag now means
    # NO EVIDENCE AND NO WRITE for this call.
    local root self saved_registry
    mkdir -p "$WORK/regread-error/proj18"
    root="$(realpath "$WORK/regread-error/proj18")"
    self="$WORK/regread-error-self.txt"
    printf 'proj18-handle\nrepo=proj18\n' > "$self"

    saved_registry="$(cat "$REGISTRY")"
    printf 'not valid json {{{' > "$REGISTRY"

    context_in "$root" "$self"
    # Restore the registry BEFORE any assertion can return early and leave
    # every later case running against a broken registry.
    printf '%s' "$saved_registry" > "$REGISTRY"

    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "could not read/parse the sot-comm registry" || { echo "  missing the registry-read-error refusal: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  SELF-HEALED despite an unreadable registry: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (registry unreadable -> no heal)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  self-file was mutated despite an unreadable registry (NO WRITE required): $(cat "$self")"; return 1; }
    return 0
}

case_legacy_selffile_registry_root_disagreement_refuses_heal() {
    # Sharpest finding in the round-1 review (finding 2): basename-only
    # healing can CERTIFY THE WRONG CHECKOUT. The reviewer reproduced this
    # live — a legacy self-file (repo= matches, no root=) read from
    # checkout B, while the registry ALREADY records that same handle's
    # root as checkout A (a different directory sharing B's basename) —
    # and the old code healed the self-file onto checkout B's root
    # anyway, recreating the exact alias root= was added to eliminate.
    # A registry root DISAGREEMENT must reject the heal outright — a
    # basename match can never override contrary registry evidence.
    local rootA rootB base self seed_obj
    mkdir -p "$WORK/regdisagree/checkoutA/proj8" "$WORK/regdisagree/checkoutB/proj8"
    rootA="$(realpath "$WORK/regdisagree/checkoutA/proj8")"
    rootB="$(realpath "$WORK/regdisagree/checkoutB/proj8")"
    base="proj8"

    # The registry already knows this handle as checkout A's (a real prior
    # join from there).
    seed_obj="$(jq -n --arg repo "$base" --arg root "$rootA" \
        '{host:"h",tmux:"",pane_id:"",repo:$repo,root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    with_lock registry_put "proj8-handle" "$seed_obj"

    self="$WORK/regdisagree-self.txt"
    printf 'proj8-handle\nrepo=%s\n' "$base" > "$self"

    # Read from checkout B — repo= matches (both share basename "proj8"),
    # but the registry's root for this handle is checkout A's. Must
    # refuse to heal, never certify checkout B as this handle's root.
    context_in "$rootB" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness/refusal notice: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  SELF-HEALED against a disagreeing registry root — this is the reproduced wrong-checkout certification bug: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (registry disagrees on root; must not adopt checkout B)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  self-file was mutated despite the registry disagreement (must be left untouched): $(cat "$self")"; return 1; }
    [ "$(registry_root "proj8-handle")" = "$rootA" ] \
        || { echo "  registry root for @proj8-handle changed: $(registry_root "proj8-handle"), want $rootA (untouched)"; return 1; }
    return 0
}

case_legacy_selffile_unknown_root_registry_row_still_heals_on_repo_match() {
    # Sibling of the disagreement case above, proving the OTHER half of the
    # ruling matrix: a registry row that EXISTS for this handle but carries
    # no root of its own (a legacy registry row, predating root=) offers no
    # contrary evidence — it is treated the same as "no row at all", and
    # the repo-basename match still heals (documented residual ambiguity).
    local root base self legacy_reg_obj
    mkdir -p "$WORK/regunknown/proj11"
    root="$(realpath "$WORK/regunknown/proj11")"
    base="proj11"

    legacy_reg_obj="$(jq -n --arg repo "$base" \
        '{host:"other",tmux:"",pane_id:"",repo:$repo,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    jq --arg n "proj11-handle" --argjson o "$legacy_reg_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
        && mv "$REGISTRY.tmp" "$REGISTRY"
    [ "$(registry_has_root_key "proj11-handle")" = "no" ] \
        || { echo "  setup bug: seeded registry row unexpectedly has a root key"; return 1; }

    self="$WORK/regunknown-self.txt"
    printf 'proj11-handle\nrepo=%s\n' "$base" > "$self"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" || { echo "  missing self-heal notice (an unknown-root registry row must not block healing on repo match): $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "proj11-handle" ] || { echo "  NAME=$CTX_NAME, want proj11-handle"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -ge 3 ] || { echo "  self-file not backfilled to v2: $(cat "$self")"; return 1; }
    return 0
}

case_ancient_oneline_with_matching_registry_heals() {
    # Ancient one-line format (no repo=, no root= — pre-#68): the reviewer
    # flagged this as broader than the repo=-matching legacy case, since it
    # carries NO evidence of its own — not even a basename. It must heal
    # ONLY when the registry independently corroborates a matching root.
    local root base self seed_obj
    mkdir -p "$WORK/ancient-match/proj12"
    root="$(realpath "$WORK/ancient-match/proj12")"
    base="proj12"

    seed_obj="$(jq -n --arg repo "$base" --arg root "$root" \
        '{host:"h",tmux:"",pane_id:"",repo:$repo,root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    with_lock registry_put "ancient-handle-match" "$seed_obj"

    self="$WORK/ancient-match-self.txt"
    printf 'ancient-handle-match\n' > "$self"   # ONE line: no repo=, no root=

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" || { echo "  missing self-heal notice (ancient one-line WITH a matching-root registry row must heal): $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "ancient-handle-match" ] || { echo "  NAME=$CTX_NAME, want ancient-handle-match"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -ge 3 ] || { echo "  ancient one-line self-file not upgraded to v2: $(cat "$self")"; return 1; }
    return 0
}

case_ancient_oneline_without_registry_match_discarded() {
    # Mirror of the above: an ancient one-line self-file with NO
    # corroborating registry row (absent, or an unknown/disagreeing root)
    # carries no evidence of its own at all and must be discarded, not
    # healed on nothing.
    local root self
    mkdir -p "$WORK/ancient-nomatch/proj13"
    root="$(realpath "$WORK/ancient-nomatch/proj13")"

    self="$WORK/ancient-nomatch-self.txt"
    printf 'ancient-handle-nomatch\n' > "$self"   # ONE line, no registry row exists for this handle at all

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing discard notice for an ancient one-line file with no registry corroboration: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  SELF-HEALED an ancient one-line file with no corroborating registry evidence: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (no evidence to heal on)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 1 ] || { echo "  ancient one-line self-file was mutated despite no corroboration: $(cat "$self")"; return 1; }
    return 0
}

case_self_heal_write_failure_reported_loudly_file_intact() {
    # Codex review round-1 finding 3: the pre-fix in-place `>` truncation
    # ignored a failed write entirely — with a read-only self-file it
    # exited 0 and printed "self-healed" while the file remained legacy.
    # The fix (comm-lib.sh's sot_write_self_file) writes via a
    # same-directory temp file + checked `mv` instead — which means a
    # read-only TARGET FILE no longer blocks anything (`mv`/rename(2) only
    # needs write permission on the DIRECTORY, not on the file being
    # replaced). Reproducing a write failure under the NEW mechanics means
    # making the self-file's DIRECTORY unwritable (mktemp then fails to
    # create the temp file there), not the file itself.
    if [ "$(id -u)" -eq 0 ]; then
        echo "  SKIP: running as root — permission bits don't block root, so a write failure can't be reproduced this way"
        return 2
    fi
    local dir root self rc
    mkdir -p "$WORK/heal-write-fail-root/projRO"
    root="$(realpath "$WORK/heal-write-fail-root/projRO")"
    dir="$WORK/heal-write-fail-selfdir"
    mkdir -p "$dir"
    self="$dir/legacy-self.txt"
    printf 'readonly-dir-handle\nrepo=projRO\n' > "$self"
    chmod 500 "$dir"   # r-x: mktemp can no longer create a sibling temp file here

    context_in "$root" "$self"
    rc="$CTX_RC"
    chmod 700 "$dir"   # restore BEFORE any assertion can return early and leave an unwritable dir behind for the suite's own cleanup

    [ "$rc" -eq 0 ] || { echo "  comm-context.sh exited $rc (must still succeed for THIS call even though the heal write failed): $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "FAILED to self-heal" || { echo "  missing the loud write-failure notice: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed for" && { echo "  claimed success (\"self-healed for\") despite the write failing: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "readonly-dir-handle" ] || { echo "  NAME=$CTX_NAME, want readonly-dir-handle (the identity is still valid for THIS call even though persisting the heal failed)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  self-file was mutated despite the write failing (must be left untouched): $(cat "$self")"; return 1; }
    ! ls "$dir"/*.tmp.* >/dev/null 2>&1 || { echo "  a stray temp file was left behind: $(ls "$dir")"; return 1; }
    return 0
}

case_nopane_selffile_shared_across_repos_not_healed() {
    # Coordinator addendum, item 6: a shell with NO workspace row collapses to
    # ONE self-file slot per host ("<host>__nopane.txt", see
    # comm-context.sh) shared by every such shell on this host. The
    # repo=/root= check is what makes that sharing safe — a background
    # shell in a DIFFERENT repo reading the same slot back must be
    # discarded, never healed into adopting the other repo's identity.
    #
    # Codex review round-2 finding A tightened this further: for the
    # SHARED nopane slot specifically (unlike a pane-keyed file), even a
    # MATCHING repo= basename is not enough evidence to heal — the file's
    # own name below uses the real "__nopane.txt" production suffix so
    # comm-context.sh's IS_NOPANE detection actually applies to it, and
    # repoA's own re-read now seeds a corroborating registry row first (a
    # basename match alone no longer heals a nopane slot).
    local rootA rootB self seed_obj
    mkdir -p "$WORK/nopane-cross-repo/repoA" "$WORK/nopane-cross-repo/repoB"
    rootA="$(realpath "$WORK/nopane-cross-repo/repoA")"
    rootB="$(realpath "$WORK/nopane-cross-repo/repoB")"
    self="$WORK/nopane-cross-repo/${HOST}__nopane.txt"

    # repoA's session previously claimed the shared nopane slot (legacy,
    # pre-root=, so this ALSO exercises the self-heal boundary: it must
    # heal for repoA, never for repoB).
    printf 'repoA-handle\nrepo=repoA\n' > "$self"

    context_in "$rootB" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  repoB read NAME=$CTX_NAME, want empty (must not adopt repoA's identity)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  shared nopane self-file was mutated by the mismatched read: $(cat "$self")"; return 1; }

    # repoA reading its OWN slot back: now REQUIRES registry corroboration
    # (round-2 finding A) — seed a matching-root row before it can heal.
    seed_obj="$(jq -n --arg repo "repoA" --arg root "$rootA" \
        '{host:"h",tmux:"",pane_id:"",repo:$repo,root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    with_lock registry_put "repoA-handle" "$seed_obj"

    context_in "$rootA" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  repoA re-read exited $CTX_RC: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "repoA-handle" ] || { echo "  repoA re-read NAME=$CTX_NAME, want repoA-handle"; return 1; }
    contains "$CTX_ERR" "self-healed" || { echo "  repoA re-read missing self-heal notice: $CTX_ERR"; return 1; }
    return 0
}

case_nopane_same_basename_different_root_discarded() {
    # Codex review round-2 finding A: two checkouts share a basename ("
    # sharedname") and this project's repo= matches BOTH — but the shared
    # nopane slot has no registry evidence tying it to either, so a
    # basename match alone must NOT heal it (unlike a pane-keyed file,
    # where ruling 2's original matrix still heals on repo match alone).
    local rootA rootB self
    mkdir -p "$WORK/nopane-samebase/siteA/sharedname" "$WORK/nopane-samebase/siteB/sharedname"
    rootA="$(realpath "$WORK/nopane-samebase/siteA/sharedname")"
    rootB="$(realpath "$WORK/nopane-samebase/siteB/sharedname")"
    self="$WORK/nopane-samebase/${HOST}__nopane.txt"
    printf 'sharedname-handle\nrepo=sharedname\n' > "$self"   # claimed from rootA, legacy, no registry row

    context_in "$rootB" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness/refusal notice for a same-basename different-root nopane read: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  SELF-HEALED a same-basename different-checkout nopane self-file — this is exactly the aliasing round-2 finding A closes: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (nopane + basename match alone must not heal)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  nopane self-file was mutated despite no registry corroboration: $(cat "$self")"; return 1; }
    return 0
}

case_nopane_same_basename_non_repo_cwd_discarded() {
    # Sibling of the above: a same-basename NON-repo cwd (two unrelated
    # scratch dirs both literally named "sometmpdir") must be equally
    # unable to heal the shared nopane slot on basename alone.
    local rootA scratchB self
    mkdir -p "$WORK/nopane-nonrepo/siteA/sometmpdir" "$WORK/nopane-nonrepo/scratch/sometmpdir"
    rootA="$(realpath "$WORK/nopane-nonrepo/siteA/sometmpdir")"
    scratchB="$(realpath "$WORK/nopane-nonrepo/scratch/sometmpdir")"
    self="$WORK/nopane-nonrepo/${HOST}__nopane.txt"
    printf 'sometmpdir-handle\nrepo=sometmpdir\n' > "$self"

    context_in "$scratchB" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness/refusal notice for a same-basename non-repo-cwd nopane read: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  SELF-HEALED a same-basename non-repo-cwd nopane self-file: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (nopane + basename match alone must not heal, even outside a repo)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  nopane self-file was mutated despite no registry corroboration: $(cat "$self")"; return 1; }
    return 0
}

case_nopane_with_matching_registry_root_heals() {
    # Positive path for round-2 finding A: the shared nopane slot DOES
    # heal once the registry independently corroborates a matching root —
    # the tightened rule requires evidence, it doesn't forbid healing
    # outright.
    local root self seed_obj
    mkdir -p "$WORK/nopane-corroborated/proj14"
    root="$(realpath "$WORK/nopane-corroborated/proj14")"
    self="$WORK/nopane-corroborated/${HOST}__nopane.txt"
    printf 'proj14-handle\nrepo=proj14\n' > "$self"

    seed_obj="$(jq -n --arg repo "proj14" --arg root "$root" \
        '{host:"h",tmux:"",pane_id:"",repo:$repo,root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    with_lock registry_put "proj14-handle" "$seed_obj"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" || { echo "  missing self-heal notice for a registry-corroborated nopane slot: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "proj14-handle" ] || { echo "  NAME=$CTX_NAME, want proj14-handle"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -ge 3 ] || { echo "  nopane self-file not backfilled to v2: $(cat "$self")"; return 1; }
    return 0
}

case_nopane_selffile_from_non_repo_cwd_not_healed_and_send_refuses() {
    # Tiny addendum (coordinator, field-corroborated): a background shell
    # cd'd OUTSIDE any repo entirely (REPO then reads as the cwd's own
    # basename, e.g. a scratchpad dir) trips the SAME repo-mismatch discard
    # from the other side — must not be healed either, and a send from
    # there must refuse loudly (item 5) rather than stamp a placeholder
    # sender. The fix for an operator here is "run from the repo", not
    # "rejoin".
    local self scratch
    mkdir -p "$WORK/nopane-scratch/some-scratchpad"
    scratch="$(realpath "$WORK/nopane-scratch/some-scratchpad")"
    self="$WORK/nopane-scratch/${HOST}__nopane.txt"
    printf 'repoA-handle\nrepo=repoA\n' > "$self"

    context_in "$scratch" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  non-repo-cwd read NAME=$CTX_NAME, want empty (must not adopt repoA's identity)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  self-file was mutated by a non-repo-cwd read: $(cat "$self")"; return 1; }

    # A send from here must refuse loudly (item 5), not stamp a
    # synthesized "unknown-<host>" sender.
    local send_out send_err send_rc errfile
    errfile="$WORK/send-scratch.err"
    send_out="$(cd "$scratch" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SEND" @somebody "hello" 2>"$errfile")"
    send_rc=$?
    send_err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$send_rc" -ne 0 ] || { echo "  comm-send.sh succeeded with no identity: $send_out"; return 1; }
    contains "$send_err" "unknown-" && { echo "  comm-send.sh still stamped a synthesized unknown-<host> sender: $send_err"; return 1; }
    contains "$send_err" "identity did not resolve" || { echo "  missing identity-refusal message: $send_err"; return 1; }
    return 0
}


case_context_host_part_follows_the_raw_host_rule() {
    # HANDLE_HOST, the derived handle's host component and the old slot's key, is the raw host:
    # SOT_COMM_TEST_HOST when non-empty, else `hostname -s` (case kept), else a plain `hostname`.
    # HOST, the registry's and the unpinned slot's host, is the declared host (sot_host): SOT_SELF_HOST, else the
    # lowercased first label of the same hostname.
    local bin="$WORK/rawhost-bin" self="$WORK/rawhost-self.txt" out
    mkdir -p "$bin" "$WORK/rawhost-cwd"
    printf '#!/bin/sh\nif [ "${1:-}" = "-s" ]; then echo Raw-Host; else echo plain-host; fi\n' > "$bin/hostname"
    chmod +x "$bin/hostname"
    ctx_field() {  # FIELD [ENV...] : one comm-context.sh field under the fake hostname
        local f="$1"; shift
        (cd "$WORK/rawhost-cwd" && env -u SOT_COMM_TEST_HOST -u SOT_SELF_HOST PATH="$bin:$PATH" SOT_COMM_SELF_FILE="$self" "$@" "$CONTEXT" 2>/dev/null | sed -n "s/^$f=//p")
    }
    out="$(ctx_field HANDLE_HOST)"; [ "$out" = "Raw-Host" ] || { echo "  unset: HANDLE_HOST=$out, want Raw-Host"; return 1; }
    out="$(ctx_field HOST)"; [ "$out" = "raw-host" ] || { echo "  unset: HOST=$out, want the declared raw-host"; return 1; }
    out="$(ctx_field HOST SOT_SELF_HOST=Declared-Box)"; [ "$out" = "Declared-Box" ] || { echo "  override: HOST=$out, want Declared-Box verbatim"; return 1; }
    printf '#!/bin/sh\nif [ "${1:-}" = "-s" ]; then exit 1; else echo plain-host; fi\n' > "$bin/hostname"
    out="$(ctx_field HANDLE_HOST)"; [ "$out" = "plain-host" ] || { echo "  failing -s: HANDLE_HOST=$out, want plain-host"; return 1; }
    out="$(ctx_field HOST)"; [ "$out" = "plain-host" ] || { echo "  failing -s: HOST=$out, want plain-host"; return 1; }
    out="$(ctx_field HANDLE_HOST SOT_COMM_TEST_HOST=pinned)"; [ "$out" = "pinned" ] || { echo "  pinned: HANDLE_HOST=$out, want pinned"; return 1; }
    printf '#!/bin/sh\nexit 1\n' > "$bin/hostname"
    (cd "$WORK/rawhost-cwd" && env -u SOT_COMM_TEST_HOST -u SOT_SELF_HOST PATH="$bin:$PATH" SOT_COMM_SELF_FILE="$self" "$CONTEXT" >/dev/null 2>&1) \
        && { echo "  a failing hostname still produced an identity"; return 1; }
    return 0
}
