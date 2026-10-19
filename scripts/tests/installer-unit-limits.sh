# installer-unit-limits.sh -- sourced by installer-state.sh (its check helpers, the library's functions and $WORK), never
# run: the two bounds a launcher or the unit render puts on what it writes. A start's log prune counts the log the start is
# about to create, and each render refuses a path its file would splice as syntax.

# ---------------------------------------------------------------------------
case_start "prune_counts_the_log_about_to_be_created"
# Five old logs leave four: the start about to create a log is the fifth.
d="$WORK/logs-keep"; mkdir -p "$d"
for i in 1 2 3 4 5; do printf 'x\n' > "$d/sotd.20200101-00000$i-000Z-$((99999980 + i)).log"; done
sot_prune_logs "$d"
check "a prune leaves SOT_LOG_KEEP-1 old logs for the one about to be created" "4" \
    "$(ls "$d" | wc -l | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "renders_refuse_syntax_in_a_path"
# A prefix or home holding a character a unit, a launcher or a shell line would read as syntax (the library's one list) is
# refused by each render before its own write: the old file stays whole, no temp file is left, and the line names what
# (prefix or home) and the character (a word for space, tab and newline).
UNIT_TEMPLATE="$(dirname "$0")/../../deploy/sotd.service"
nl='
'
for ch in '%' '\' "'" '"' '$' '`' '&' '|' ';' ' ' '	' "$nl"; do
    case "$ch" in ' ') word=space ;; '	') word=tab ;; "$nl") word=newline ;; *) word="$ch" ;; esac
    for which in prefix home; do
        if [ "$which" = prefix ]; then rp="$WORK/p${ch}q"; rh="$WORK/home"; else rp="$WORK/prefix"; rh="$WORK/h${ch}q"; fi
        for render in unit launch; do
            RB="$WORK/rendered-refused.out"; printf 'old file\n' > "$RB"
            rrc=0
            if [ "$render" = unit ]; then rerr="$(HOME="$rh" render_sotd_unit "$rp" "$UNIT_TEMPLATE" "$RB" 2>&1 >/dev/null)" || rrc=$?
            else rerr="$(HOME="$rh" render_sot_launch "$rp" linux-x86_64 "$RB" 2>&1 >/dev/null)" || rrc=$?; fi
            check "the $render render refuses a $which holding $word" "1" "$rrc"
            check "the refusal names the $which" "1" "$(printf '%s' "$rerr" | grep -cF -- "ERROR: the $which " || true)"
            check "the refusal names $word" "1" "$(printf '%s' "$rerr" | grep -cF -- " holds $word," || true)"
            check "the $render render left the old file whole and no temp file" "old file 1" \
                "$(cat "$RB") $(ls "$WORK" | grep -c '^rendered-refused' || true)"
        done
    done
done
rm -f "$WORK/rendered-refused.out"

# ---------------------------------------------------------------------------
case_start "one_path_predicate"
# The library's refusal has two byte-identical copies, install.sh's front gate and sot-apply's check before the flip.
refused_text() { sed -n '/^sot_path_refused() {/,/^}/p' "$1"; }
check "the library defines sot_path_refused" "yes" "$([ -n "$(refused_text "$(dirname "$0")/../lib/sot-daemon.sh")" ] && echo yes || echo no)"
for f in install.sh sot-apply.sh; do
    check "$f's sot_path_refused is byte-identical to the library's" "$(refused_text "$(dirname "$0")/../lib/sot-daemon.sh")" "$(refused_text "$(dirname "$0")/../$f")"
done
check "no other script defines it" "3" "$(grep -rl '^sot_path_refused() {' "$(dirname "$0")/.." | wc -l | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "path_refusal_under_dash"
# sot-apply runs the library under dash, whose echo reads backslash escapes: a path holding \c or \n is exactly what is
# refused, and its line must still come out whole, as one line naming the character.
if ! command -v dash >/dev/null 2>&1; then
    printf '  SKIP the dash cases (no dash here)\n'
else
    for odd in '/opt/a\cb' '/opt/x\new'; do
        derr="$(dash -c '. "$1"; sot_path_refused prefix "$2"' dash "$(dirname "$0")/../lib/sot-daemon.sh" "$odd" 2>&1 >/dev/null)" || true
        check "dash: $odd gives one line" "1" "$(printf '%s\n' "$derr" | wc -l | tr -d ' ')"
        check "dash: the line holds the path whole and names the character" "1" \
            "$(printf '%s\n' "$derr" | grep -cF -- "ERROR: the prefix $odd holds \\," || true)"
    done
    # A log's name carries the folder's: one that holds \c must come out whole in the one-line "kept old log".
    log_d="$WORK/l\\cdir"; mkdir -p "$log_d"
    for i in 1 2 3 4 5 6; do printf 'x\n' > "$log_d/sotd.20200101-00000$i-000Z-$((99999980 + i)).log"; done
    chmod 0555 "$log_d"
    derr="$(dash -c '. "$1"; sot_prune_logs "$2"' dash "$(dirname "$0")/../lib/sot-daemon.sh" "$log_d" 2>&1 >/dev/null)" || true
    chmod 0755 "$log_d"
    check "dash: a log folder holding \\c is named whole in the kept-old-log line" "1" \
        "$(printf '%s\n' "$derr" | grep -cF -- "kept old log $log_d/sotd.20200101-000001-000Z-99999981.log: " || true)"
fi
