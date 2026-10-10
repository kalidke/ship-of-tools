# test-join-disambiguation.sh part: sot_jq_rawfile and the --arg allowlist (sourced in order by the entry).

case_jq_rawfile_helper_round_trips_leading_slash_value() {
    # capsule-comm-identity fix, item B (MSYS2 argv-conversion guard): on
    # Windows git-bash, a NATIVE jq.exe rewrites any ARGV ELEMENT that
    # starts with "/" into a Windows path before jq ever sees it — a
    # message body or a project root passed via `--arg` can legitimately
    # start with "/" and arrives corrupted. This suite runs on LINUX,
    # which cannot reproduce that MSYS2-only conversion (it only fires for
    # a native, non-MSYS jq.exe) — what IS provable here is the fix's own
    # mechanics: sot_jq_rawfile's temp file round-trips a leading-slash
    # value through `jq --rawfile` byte-for-byte, which is exactly what
    # closes the bug wherever it actually runs (the ORIGINAL corruption is
    # unreproducible on this platform; the FIX is not).
    local val f out
    val="/sot-session-start with a leading slash and trailing text"
    f="$(sot_jq_rawfile "$val")" || { echo "  sot_jq_rawfile failed"; return 1; }
    [ -f "$f" ] || { echo "  sot_jq_rawfile did not create a file at the path it printed"; return 1; }
    out="$(jq -nr --rawfile v "$f" '$v')"
    rm -f "${f:?}"
    [ "$out" = "$val" ] || { echo "  round-trip mismatch: got '$out', want '$val'"; return 1; }
    return 0
}

case_jq_arg_names_are_allowlisted_against_slash_prone_values() {
    # capsule-comm-identity fix, item B follow-up (Codex round finding 3):
    # replaces a narrow three-name grep with a real audit. Every
    # `jq --arg NAME "$value"` across the comm scripts + hooks must bind a
    # NAME on this allowlist — handles/ids/hosts/timestamps/short enum-ish
    # tokens/already-relativized paths, none of which can legitimately
    # start with "/". A message body, an absolute/backend path, free
    # text, code, or a label is NOT on it and must go through
    # comm-lib.sh's sot_jq_rawfile (--rawfile) instead — see that helper's
    # own comment for the MSYS2 argv-conversion mechanism this guards
    # against. `--argjson` is exempt by construction (its value is JSON
    # text, which can never start with "/" — every valid top-level JSON
    # value starts with `{`, `[`, `"`, a digit, `t`, `f`, or `n`).
    #
    # NAME-based, not call-site-based — a real, documented limitation: a
    # NEW `--arg p ...` site that reuses an allowlisted name for a
    # genuinely risky value would NOT be caught here, only a value bound
    # under a NOT-yet-allowlisted name is. The `p`/`ws` entries here are
    # ALREADY relativized before use (sot-fe's preview/reveal PATH_ARG,
    # sot-nav.sh's REL) — a new risky path/text value should bind a
    # fresh, not-yet-allowlisted name so this test forces a deliberate
    # choice about it. `c` is sot-fe's fe_cmd — always one of a small
    # fixed set of literal verbs from a case dispatch, never raw text.
    # `ag` is comm-spawn.sh's --agent kind — validated to `claude|codex|none`
    # before it is ever bound, never raw text.
    # `acc` is an --account value, in comm-spawn.sh and in sot-fe's reauth
    # verb: BOTH validate it to the account-name rule ^[a-z0-9][a-z0-9_-]*$
    # before binding it, so it can never carry a leading "/". Reusing the
    # name is only legitimate because the validation is reused with it.
    # A line whose first non-blank character is '#' is skipped entirely
    # (a prose mention of `--arg NAME`, not a real binding).
    # `o` = comm-status.sh's turn_origin (ADR 0044): the enum user|machine,
    # set from an env var the prompt hook controls, never free text.
    # `cur` = the read cursor's content (comm-lib.sh's sot_cursor_offset and the
    # idle hook's own inlined copy): a line count, or a legacy ISO timestamp
    # being converted to one. Neither can begin with "/", and the only writer of
    # that file is comm-poll.sh.
    # `i` = the relay's own MSG_ID (ADR 0048): minted as
    # `<epoch-ns>-<pid>-<random>` in comm-relay.sh and echoed back from the
    # frame being receipted, so it is digits and dashes and can never begin
    # with "/". The message BODY on those same jq calls still goes through
    # --rawfile.
    # `one` = sot_registry_read's "a row was asked for" flag, `${1+1}`: "1"
    # or "", never "/".
    local allow=" n t ts from to repo me w h b host tmux pane an s id f st u m c l nonce ws p ag o acc cur i one "
    local bad="" file name line match comment_lines
    [ -f "$SCRIPTS_DIR/comm-status-idle.sh" ] || { echo "  the staged bin holds no hooks"; return 1; }
    for file in "$SCRIPTS_DIR"/*.sh "$SCRIPTS_DIR/sot-fe"; do
        [ -f "$file" ] || continue
        # Line numbers that are FULL comment lines (first non-blank char
        # '#') — a prose mention of `--arg NAME` in a comment (this
        # helper's own doc, or a fix note) must not count as a real jq
        # binding.
        comment_lines=" $(grep -nE '^[[:space:]]*#' "$file" | cut -d: -f1 | tr '\n' ' ') "
        while IFS=: read -r line match; do
            case "$comment_lines" in
                *" $line "*) continue ;;
            esac
            name="$(printf '%s' "$match" | sed -E 's/^--arg[[:space:]]+//')"
            case "$allow" in
                *" $name "*) : ;;
                *) bad="$bad
  $(basename "$file"):$line binds --arg $name (not on the allowlist)" ;;
            esac
        done < <(grep -onE -- '--arg[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' "$file")
    done
    [ -z "$bad" ] || { echo "  non-allowlisted --arg bindings found:$bad"; return 1; }
    return 0
}

