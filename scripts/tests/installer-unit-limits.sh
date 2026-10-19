# installer-unit-limits.sh -- sourced by installer-state.sh (its check helpers, the library's functions and $WORK), never
# run: the two bounds a launcher or the unit render puts on what it writes. A start's log prune counts the log the start is
# about to create, and the unit render refuses a path systemd or the unit's shell would read as syntax.

# ---------------------------------------------------------------------------
case_start "prune_counts_the_log_about_to_be_created"
# Five old logs leave four: the start about to create a log is the fifth.
d="$WORK/logs-keep"; mkdir -p "$d"
for i in 1 2 3 4 5; do printf 'x\n' > "$d/sotd.20200101-00000$i-000Z-$((99999980 + i)).log"; done
sot_prune_logs "$d"
check "a prune leaves SOT_LOG_KEEP-1 old logs for the one about to be created" "4" \
    "$(ls "$d" | wc -l | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "unit_render_refuses_syntax_in_a_path"
# A prefix or home holding a character systemd or the unit's bash -c quoting reads (% expanded, \ unescaped, ' ends the
# quoting, " ends the word, $ expands) is refused before any write, naming the character.
UNIT_TEMPLATE="$(dirname "$0")/../../deploy/sotd.service"
for ch in '%' '\' "'" '"' '$'; do
    for which in prefix home; do
        RB="$WORK/rendered-refused.service"; printf 'old unit\n' > "$RB"
        if [ "$which" = prefix ]; then rp="$WORK/p${ch}q"; rh="$WORK/home"; else rp="$WORK/prefix"; rh="$WORK/h${ch}q"; fi
        rrc=0; rerr="$(HOME="$rh" render_sotd_unit "$rp" "$UNIT_TEMPLATE" "$RB" 2>&1 >/dev/null)" || rrc=$?
        check "a $which holding $ch is refused" "1" "$rrc"
        check "the refusal names $ch and the $which" "1" "$(printf '%s' "$rerr" | grep -cF -- "holds $ch" || true)"
        check "the $which holding $ch left the old unit whole and no temp file" "old unit 1" \
            "$(cat "$RB") $(ls "$WORK" | grep -c '^rendered-refused' || true)"
    done
done
rm -f "$WORK/rendered-refused.service"
