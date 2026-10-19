# installer-path-refusal.sh -- sourced by installer-entry.sh (its fixture, runner and helpers), never run: install.sh run end
# to end with a home holding a character the generated files would splice as syntax. The front gate refuses it before any
# write (the prefix tree is the same bytes afterward) and names the character, on the systemd path and with --no-service,
# and a --prefix that is clean under such a home is refused for the home.

# ---------------------------------------------------------------------------
case_start "entry_refuses_a_home_the_generated_files_cannot_carry"
for ch in '%' '$'; do
    for mode in service no-service; do
        d="$WORK/ref-$mode-$(printf '%s' "$ch" | od -An -tx1 | tr -d ' \n')"; entry_fixture "$d"
        mv "$d/home" "$d/h${ch}q"
        P="$d/h${ch}q/.local/share/sot"
        tree_before="$(tree_of "$d/h${ch}q/.local" "$d/comm")"
        args=(--be-only); [ "$mode" = no-service ] && args+=(--no-service)
        ENTRY_RC=0
        ENTRY_OUT="$(env -i HOME="$d/h${ch}q" PATH="$d/stubs:$ENTRY_TOOLS" CTL="$d/ctl" SOT_COMM_HOME="$d/comm" SOT_SELF_HOST=entrybox \
            TMPDIR="$d" bash "$ENTRY_SRC/scripts/install.sh" --archive "$d/rel" "${args[@]}" 2>&1)" || ENTRY_RC=$?
        check "a home holding $ch ($mode) is refused" "1" "$ENTRY_RC"
        check "the line names the prefix and $ch" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -cF "ERROR: the prefix $P holds $ch," || true)"
        check "the run ends in one FAIL verdict" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -c '^FAIL: ' || true)"
        check "nothing was written before the refusal ($mode, $ch)" "same" \
            "$([ "$tree_before" = "$(tree_of "$d/h${ch}q/.local" "$d/comm")" ] && echo same || echo differ)"
        # The prefix is clean here; the home is not.
        ENTRY_RC=0
        ENTRY_OUT="$(env -i HOME="$d/h${ch}q" PATH="$d/stubs:$ENTRY_TOOLS" CTL="$d/ctl" SOT_COMM_HOME="$d/comm" SOT_SELF_HOST=entrybox \
            TMPDIR="$d" bash "$ENTRY_SRC/scripts/install.sh" --archive "$d/rel" --prefix "$d/cleanprefix" "${args[@]}" 2>&1)" || ENTRY_RC=$?
        check "a clean --prefix under a home holding $ch ($mode) is refused" "1" "$ENTRY_RC"
        check "the line names the home and $ch" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -cF "ERROR: the home $d/h${ch}q holds $ch," || true)"
        check "no prefix was created ($mode, $ch)" "absent" "$([ -e "$d/cleanprefix" ] && echo present || echo absent)"
    done
done
