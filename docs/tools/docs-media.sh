#!/usr/bin/env bash
# docs-media.sh — headless docs stills + motion loops from the REAL frontend.
#
#   docs/tools/docs-media.sh stills [name...]   # 1920x1080 PNG stills
#   docs/tools/docs-media.sh loops  [name...]   # mp4 + webm + poster
#   docs/tools/docs-media.sh features [name...] # one short loop per feature
#   docs/tools/docs-media.sh all                # stills, loops and features
#   docs/tools/docs-media.sh agent              # the agent takes: one real Claude Code session
#   docs/tools/docs-media.sh agent-cut          # cut the kept agent takes into loops and stills
#   docs/tools/docs-media.sh list               # the still + loop matrix
#
# Linux only, no display or GPU needed: the frontend renders through Mesa's
# software Vulkan (lavapipe) into a private Xvfb, ffmpeg x11grab records the
# screen, and a ~40-line ctypes XTest driver (python3 + libX11 + libXtst, no
# extra packages) presses the keys a user would.
#
# Isolation — the demo must never show or touch the maintainer's own state:
#   * every process runs under `env -i` with a throwaway HOME and XDG_* dirs
#     (no SOT_* variable survives, so no real daemon, relay or session list
#     is reachable), inside a user+UTS+mount namespace whose hostname is
#     "demo" and whose /home is a tmpfs holding only /home/demo (the
#     throwaway HOME) and the invoking user's own home (for the julia and
#     sot binaries);
#   * the project is a copy of docs/fixtures/DemoProject at
#     /home/demo/DemoProject, the path every frame shows;
#   * the scratch sotd, its capsule rows, the frontend, Xvfb and ffmpeg are
#     started here and all reaped by the EXIT trap. The invoking user's home
#     is mounted read-only inside the namespace, and the trap checks that no
#     demo handle reached the real ~/.sot-comm/registry.json.
# The frontend's window geometry comes from its persisted state file:
# seeding window_w/h = 1920x1080 is what makes it fill the Xvfb screen
# (borderless fullscreen needs a window manager).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIXTURE="$REPO/docs/fixtures/DemoProject"
OUT="$REPO/docs/src/assets/media"
# sot + sotd + sot-capsule: SOT_MEDIA_BIN, else a build pinned for the docs
# media (dev/output/docs-media-bin, gitignored — every loop and still should
# come from one build), else the installed one.
PINNED_BIN="$REPO/dev/output/docs-media-bin"
[ -x "$PINNED_BIN/sot" ] || PINNED_BIN="$HOME/.local/share/sot/bin"
BIN="${SOT_MEDIA_BIN:-$PINNED_BIN}"
[ -x "$BIN/sot" ] || BIN="$REPO/rust/target/release"
FONT_SCALE="${SOT_MEDIA_FONT_SCALE:-1.5}"   # legible at ~1200 px display width
W=1920; H=1080; FPS=30
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-docs-media.XXXXXX")"
DHOME=/home/demo                 # the HOME every demo process sees ...
HOSTHOME="$WORK/home"            # ... and where it lives on this box
DEMO="$DHOME/DemoProject"        # the project, as the demo sees it
REAL_REGISTRY="$HOME/.sot-comm/registry.json"

say() { printf '\033[1;36m==\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ---- still matrix ------------------------------------------------------------
# name|FE flags|settle seconds|steps (';'-separated; see steps())
# Every FE is switched onto the live DemoProject row (attach_row), so the llm
# column is that row's shell. The row switch that attaches the llm pane re-roots the Files tree on the
# row's project with the cursor on README.md, so every still walks from
# there: SRC = src/DemoProject.jl, DATA = data/'s first entry.
SRC="key Up;key Up;key Up;sleep 0.5;key Right;sleep 1;key Down"
DATA="key Up;key Up;key Up;key Up;key Up;sleep 0.5;key Right;sleep 1;key Down"
SCRIPT="key Up;key Up;key Up;key Up;sleep 0.5;key Right;sleep 1;key Down"
# Sessions mode, the current host expanded, the pane maximized (Alt+=).
SESSIONS_PANE="key s;sleep 1;key Down;key Right;sleep 1;key alt+equal;sleep 2"
# Ctrl+L leaves "scrollback cleared" in the status line; Escape (focus to
# the tree) and a round trip through Sessions mode restore the plain
# connection status, and Ctrl+Down focuses the open drawer again.
CLEARED="key ctrl+l;sleep 0.5;key Escape;sleep 0.5;key s;sleep 1;key f;sleep 1"
# The first run in a fresh REPL compiles; the shot keeps the second, the
# sub-second timing a user sees from then on (and the loops show).
RUN_TWICE="type include(\"scripts/route.jl\");key Return;sleep 45;$CLEARED;key ctrl+Down;sleep 1;type include(\"scripts/route.jl\");key Return;sleep 6"
STILLS=(
  "nav-files|--start-mode files|10|$SRC;sleep 3"
  "preview-math|--start-mode files|10|$DATA;sleep 6"
  "preview-hdf5|--start-mode files|10|$DATA;sleep 3;key Down;sleep 35"
  "concept-stale|--start-mode files --start-maximized|10|$SRC;sleep 6"
  "repl-figure|--start-mode files|10|$SCRIPT;sleep 2;key ctrl+j;sleep 8;$RUN_TWICE"
  # Last: a declared blocked or done outlives the idle reset each still does.
  "state-colors|--start-mode files|10|rows_state analysis:working figures:waiting gpu-train:blocked survey:done;sleep 3;$SESSIONS_PANE"
)
# Every still is a tight crop (WxH+X+Y of the window, region_vf) of the
# region it is about, and none shows the fe/be version label on the
# window's bottom border: BODY_H ends a full-height crop at y 1014, above
# it (preview-math, right of the label, reaches only the pane border). The preview stills keep only the preview pane, so the
# nav pane's status, annotation and key rows stay out; nav-files and
# repl-figure keep nav and preview (the llm column is a bare shell);
# concept-stale keeps the maximized nav pane's rows and state-colors the
# maximized Sessions pane's rows (readme/make-crops.sh crops both further).
BODY_H=998
NAVPREV_CROP="1260x$BODY_H+16+16"
declare -A STILL_CROP=([preview-math]="664x1030+612+16" [preview-hdf5]="664x590+612+16"
    [nav-files]="$NAVPREV_CROP" [repl-figure]="$NAVPREV_CROP"
    [concept-stale]="1240x436+16+16" [state-colors]="808x336+16+16")

# ---- loop matrix ---------------------------------------------------------------
# hero, navigate, repl, crop and copy come from `agent` (a real Claude Code
# session in the agent pane).
LOOPS=(sessions)

list_matrix() {
    echo "stills (${W}x${H}, font scale $FONT_SCALE) -> $OUT/<name>.png"
    for row in "${STILLS[@]}"; do
        IFS='|' read -r name flags settle keys <<<"$row"
        printf '  %-16s settle %3ss  %s%s\n' "$name" "$settle" "$flags" "${keys:+  steps: $keys}"
    done
    echo "loops -> $OUT/<name>.{mp4,webm,png}"
    printf '  %s\n' "${LOOPS[@]}"
    echo "features -> $OUT/<name>.{mp4,webm,png}"
    for row in "${FEATURES[@]}"; do printf '  %s\n' "${row%%|*}"; done
}

# ---- process bookkeeping -------------------------------------------------------
GROUPS_STARTED=()   # process-group leaders started by this script
spawn() {           # spawn <log> cmd... — own process group; sets SPAWNED
    setsid "${@:2}" >"$1" 2>&1 &
    SPAWNED=$!
    GROUPS_STARTED+=("$SPAWNED")
}
reap() {            # reap <pid> — TERM the group, then KILL stragglers
    local p="$1"
    kill -0 "$p" 2>/dev/null || return 0
    kill -TERM -- "-$p" 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
    kill -KILL -- "-$p" 2>/dev/null || true
    wait "$p" 2>/dev/null || true
}
cleanup() {
    local i now
    for ((i=${#GROUPS_STARTED[@]}-1; i>=0; i--)); do reap "${GROUPS_STARTED[$i]}"; done
    # Anything else holding the scratch dir: the setsid'd capsule supervisors
    # (their state dirs are under $WORK/state) and any stray grandchild.
    pkill -KILL -f "$WORK" 2>/dev/null || true
    # The agent take's transcript (the operator's Claude Code writes it until
    # its process is gone): only the demo project's own directory.
    case "${AGENT_TRANSCRIPT:-}" in */.claude/projects/-home-demo-DemoProject) sleep 1; rm -rf "${AGENT_TRANSCRIPT:?}" ;; esac
    # The real home is read-only inside the namespace; this is the audit.
    if [ -f "$REAL_REGISTRY" ] && jq -e --argjson h "$(printf '%s\n' "${SESSION_ROWS[@],,}" | jq -R . | jq -s .)" \
            '.agents // {} | keys | any(. as $k | $h | index($k))' "$REAL_REGISTRY" >/dev/null 2>&1; then
        echo "WARNING: a demo handle appears in $REAL_REGISTRY" >&2
    fi
    if [ -n "${SOT_MEDIA_KEEP:-}" ]; then echo "kept $WORK" >&2; else rm -rf "${WORK:?}"; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# shellcheck source=docs-media-stage.sh
. "$(dirname "${BASH_SOURCE[0]}")/docs-media-stage.sh"

# ---- stills ------------------------------------------------------------------
run_stills() {
    local only=("$@") row name flags settle ks
    for row in "${STILLS[@]}"; do
        IFS='|' read -r name flags settle ks <<<"$row"
        if [ ${#only[@]} -gt 0 ]; then
            case " ${only[*]} " in *" $name "*) ;; *) continue ;; esac
        fi
        say "still: $name (settle ${settle}s)"
        rows_state analysis:idle figures:idle gpu-train:idle survey:idle
        start_fe $flags
        sleep "$settle"; attach_row
        [ -n "$ks" ] && steps "$ks"
        grab "$OUT/$name.png"
        if [ -n "${STILL_CROP[$name]:-}" ]; then
            ffmpeg -loglevel error -y -i "$OUT/$name.png" -vf "$(region_vf "${STILL_CROP[$name]}")" "$WORK/$name.png"
            mv "$WORK/$name.png" "$OUT/$name.png"
        fi
        keys "key ctrl+l" 2>/dev/null || true   # leave the REPL clean (a no-op elsewhere)
        stop_fe
    done
}

# ---- loops -------------------------------------------------------------------
# Each loop is ONE continuous live recording, no cuts; warm-up that is not
# part of the story (kernel spawn, MathJax, PDF raster, HDF5 load, the first
# plot's compile) happens before recording starts. encode() writes mp4 +
# webm + poster.
encode() {     # encode <src> <name> <poster-second> [crop filter]
    local src="$1" name="$2" t="$3" vf="${4:-null}"
    ffmpeg -loglevel error -y -i "$src" -an -vf "$vf" -c:v libx264 -preset slow -crf 23 \
        -pix_fmt yuv420p -movflags +faststart "$OUT/$name.mp4"
    ffmpeg -loglevel error -y -i "$src" -an -vf "$vf" -c:v libvpx-vp9 -b:v 0 -crf 38 -row-mt 1 \
        -deadline good -cpu-used 2 "$OUT/$name.webm"
    ffmpeg -loglevel error -y -i "$src" -vf "$vf" -ss "$t" -frames:v 1 -update 1 "$OUT/$name.png"   # output seek: t is on the filtered timeline
}
# Warm the previews and the REPL: walk data/, run the script once (Ctrl+J
# opens and focuses the Julia drawer), clear it and close the drawer, then
# walk back (Left = parent, Left = collapse) to scripts/ — above the
# Manifest.toml and Project.toml previews, which no loop walks through.
# The drawer closes and the status line is left plain (CLEARED), so no
# take starts on a warm-up notice or timing.
WARM="sleep 2;$DATA;sleep 8;key Down;sleep 10;key Down;sleep 10;key Down;sleep 4
key ctrl+j;sleep 8;type include(\"scripts/route.jl\");key Return;sleep 60;$CLEARED;key ctrl+j;sleep 1
key Left;sleep 0.3;key Left;sleep 0.5;key Down;sleep 3"
# Open the (cleared) Julia drawer and hand focus back to the tree.
DRAWER_OPEN="key ctrl+j;sleep 2;key Escape;sleep 1"
warm() { keys "$WARM"; }

# sessions: Sessions mode on the live rows; the recording runs while each
# row's shell declares a sequence of states (the last equals the first, so
# the clip loops). The project row stays idle: it is the attached shell.
SESSION_STEPS=(
  "analysis:working figures:waiting gpu-train:done survey:idle"
  "survey:working"
  "gpu-train:blocked"
  "figures:working"
  "analysis:done"
  "gpu-train:done survey:idle"
  "analysis:working figures:waiting"
)
clip_sessions() {  # -> $WORK/sessions.mkv
    local step
    # survey works once before the take, so its idle row already carries the
    # why it will show when the clip wraps around.
    rows_state survey:working; sleep 1
    rows_state ${SESSION_STEPS[0]}
    # The Sessions pane maximized (Alt+=) at the loops' scale; encode()
    # keeps the rows and the strip (build_loop).
    start_fe --start-mode files
    sleep 10; attach_row; keys "$SESSIONS_PANE"
    rec_start "$WORK/sessions.mkv"
    sleep 1.5
    for step in "${SESSION_STEPS[@]:1}"; do rows_state $step; sleep 2.5; done
    rec_stop
    stop_fe
}

# ---- feature loops -------------------------------------------------------------
# One short take per feature, table-driven:
#   name|layout widths[/columns]|crop|setup steps (before recording)|recorded steps
# Every take starts from a fresh frontend attached to the DemoProject row
# (cursor on README.md, nav focused); the setup walks to the file and warms
# its preview, so the take shows only the feature. Ctrl+Right moves focus
# nav -> preview -> llm. After each take the llm shell's input line is
# killed and the screen cleared, so no take leaves text for the next.
# Each take keeps only the region that carries its feature (WxH+X+Y of the
# 1920x1080 window, region_vf), at the window's own pixels, above the
# bottom border's version label (BODY_H). Pane borders at the default widths: outer x
# 23..1889, y 30..1030; nav | preview at x 615, preview | llm at x 1272;
# the drawer's top border at y 489. Only modules and pin, whose story is
# the tree, keep the nav pane, and with it its status, annotation and key
# rows (internal readouts; the frontend has no switch to hide them).
TRACK="$DATA;sleep 3;key Down;sleep 1;key Down;sleep 1;key Down;sleep 4;key Down;sleep 4;key Up;sleep 3"
ZOOM_IN="key ctrl+Right;sleep 1;key =;sleep 1.2;key =;sleep 1.5"
# From track.png (data/ expanded) down to src/DemoProject.jl.
TRACK_TO_SRC="key Down;sleep 0.5;key Down;sleep 0.5;key Down;sleep 0.5;key Right;sleep 1;key Down;sleep 4"
# Modules mode from the DemoProject module down its first four functions; a
# preview trails its cursor by ~0.7 s, so each one is held 3.5 s.
MOD_WALK="key Down;sleep 3.5;key Down;sleep 3.5;key Down;sleep 3.5;key Down"
# zoom opens the drawer first (DRAWER_OPEN), which halves the preview: the
# figure then fits a landscape crop of the preview alone.
PREVIEW_CROP="664x476+612+16"
# pdf: a wide preview (its own widths) with the drawer closed: a page turn
# and back, then the page zoomed in (=; a page turn resets the zoom) and
# panned down, held, so most of the loop reads at the gallery's scale;
# cropped to the preview pane with its title row (the file and page),
# no nav sliver.
PDF_WIDTHS="0.14,0.68,0.18"; PDF_CROP="1256x$BODY_H+310+16"
# help: the drawer's rows that hold output; it keeps the
# result list only (the detail block under it names raw action ids).
HELP_CROP="500x300+20+474"
# pin: the poster is the pinned state (cursor on track.png, the preview
# still on the source), not the walk back after unpinning.
declare -A POSTER_AT=([pin]=8.5)
FEATURES=(
  "modules||$NAVPREV_CROP|key m;sleep 3;key Down;sleep 3;$MOD_WALK;sleep 3;key Up;key Up;key Up;key Up;sleep 3|sleep 2;$MOD_WALK;sleep 4"
  "pdf|$PDF_WIDTHS|$PDF_CROP|$DATA;sleep 3;key Down;sleep 1;key Down;sleep 15;key ctrl+Right;sleep 1;key n;sleep 8;key n;sleep 8;key p;sleep 2;key p;sleep 3|sleep 1;key n;sleep 3.5;key p;sleep 1.5;key =;sleep 1;key =;sleep 3;key Down;sleep 1.5;key Down;sleep 4"
  "zoom||$PREVIEW_CROP|$DRAWER_OPEN;$TRACK|sleep 1.5;$ZOOM_IN;key Right;sleep 0.8;key Up;sleep 1.5;key Escape;sleep 1;key Down;sleep 4"
  "help||$HELP_CROP|key ctrl+?;sleep 1;key ctrl+?;sleep 2|sleep 1.5;key Tab;sleep 2;type zoom;sleep 2.5;key Down;sleep 2.5;key Down;sleep 3"
  "pin||$NAVPREV_CROP|$TRACK;$TRACK_TO_SRC|sleep 1.5;key p;sleep 2.5;key Up;sleep 0.5;key Up;sleep 0.5;key Up;sleep 0.5;key Up;sleep 4;key p;sleep 3;key Up;sleep 0.5;key Up;sleep 0.5;key Up;sleep 0.5;key Up;sleep 4"
)
feature_row() {  # feature_row <name> — prints the FEATURES row, or nothing
    local row; for row in "${FEATURES[@]}"; do [ "${row%%|*}" = "$1" ] && printf '%s' "$row"; done; return 0
}
feature_vf() {   # feature_vf <name> — the take's crop as an ffmpeg filter
    local name widths crop
    IFS='|' read -r name widths crop _ <<<"$(feature_row "$1")"
    case "$crop" in *x*+*+*) region_vf "$crop" ;; *) die "$name: bad crop $crop" ;; esac
}
clip_feature() {   # clip_feature <name> -> $WORK/<name>.mkv
    local name widths crop setup take
    IFS='|' read -r name widths crop setup take <<<"$(feature_row "$1")"
    write_layout "$(w="${widths%%/*}"; echo "${w:-$WIDTHS}")" "$([[ $widths == */* ]] && echo "${widths#*/}")"
    start_fe --start-mode files
    # The extra settle keeps the Files-mode root placeholder out of the take.
    sleep 10; attach_row; keys "sleep 2"
    steps "$setup"
    rec_start "$WORK/$name.mkv"; steps "$take"; rec_stop
    stop_fe
    write_layout "$WIDTHS"
    row_type DemoProject $'\x15clear'
}

# The sessions loop keeps the rows at the top of the maximized Sessions
# pane (state-colors' crop) and the session strip under the window as two
# cards, each cut to its text, dropping the empty rows between them.
SESSIONS_ROWS="808x336+16+16"; SESSIONS_STRIP="790x44+300+1036"
SESSIONS_POSTER=10
build_loop() {
    local name="$1" len
    case "$name" in
        sessions) clip_sessions; encode "$WORK/sessions.mkv" sessions "$SESSIONS_POSTER" \
                      "$(region_vf "$SESSIONS_ROWS" "$SESSIONS_STRIP")" ;;
        *)
            [ -n "$(feature_row "$name")" ] || die "unknown loop: $name"
            clip_feature "$name"
            # The poster is the held result, a second before the end, or
            # POSTER_AT's second for a take whose end is not its point.
            len="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$WORK/$name.mkv")"
            encode "$WORK/$name.mkv" "$name" "${POSTER_AT[$name]:-$(awk -v l="$len" 'BEGIN { printf "%.1f", l - 1 }')}" "$(feature_vf "$name")"
            ;;
    esac
}
run_loops() {
    local want=("$@"); [ ${#want[@]} -gt 0 ] || want=("${LOOPS[@]}")
    local l; for l in "${want[@]}"; do say "loop: $l"; build_loop "$l"; done
}
run_features() {
    local want=("$@") row
    if [ ${#want[@]} -eq 0 ]; then for row in "${FEATURES[@]}"; do want+=("${row%%|*}"); done; fi
    run_loops "${want[@]}"
}

# ---- agent take ----------------------------------------------------------------
# `docs-media.sh agent`: every loop with a live agent pane comes from one
# real Claude Code session in the DemoProject row's agent pane: hero,
# navigate, repl, crop and copy. It runs the operator's own installed Claude
# Code, in place and as the operator, in auto permission mode (what the
# product's launcher passes), and costs a few requests. Isolation:
#   * SOT_COMM_HOOKS=off makes the product's installed Claude hooks no-op, and
#     SOT_COMM_HOME / SOT_SOCKET / SOT_WORKSPACE_ID point every comm, REPL and
#     show-result call at the scratch daemon and the demo row;
#   * only Claude Code's own config, cache and state paths are writable
#     (DAEMON_RW, bound over the read-only home); the rest stays read-only;
#   * the transcripts Claude Code writes for /home/demo/DemoProject are
#     removed when the take ends.
# The takes are kept lossless in AGENT_RAW with their times, so `agent-cut`
# re-edits them without a new take.
AGENT_RW=".claude:.claude.json:.cache/claude:.cache/claude-cli-nodejs:.local/state/claude:.local/share/claude"
AGENT_RAW="$REPO/dev/output/docs-media-agent"   # gitignored
# The agent pane is 1075 px wide in every layout, ~79 columns at font scale
# 1.5, so Claude Code never redraws for a resize between takes. Narrower
# panes were tried: at ~54 columns (a 1480 px window) its output overprints
# itself as it scrolls, so the agent loops stay 1920 wide. hero, repl and
# crop: the preview (and the drawer under it) beside the agent. navigate:
# nav and preview share the left column. copy: nav takes the preview's
# place with the drawer open, which makes the nav pane shorter than its
# body, so it scrolls the status row off the top and the annotation and
# key rows off the bottom.
AGENT_W=1920
AGENT_WIDTHS="0.44,0.56"; AGENT_COLUMNS="preview,llm"
NAV_WIDTHS="0.17,0.27,0.56"; NAV_COLUMNS="nav,preview,llm"
COPY_COLUMNS="nav,llm";   COPY_DRAWER="0.50"
AGENT_PROMPT="In scripts/route.jl, add a bar chart of the distance of each leg, saved as data/legs.png, and have the script end by returning it. Run the script in the REPL and show me the new figure in the preview. Keep your reply to a few lines."
REPL_PROMPT="Run scripts/route.jl in the REPL. Answer in one line."
CROP_PROMPT=" Which leg in this part of the figure is the shortest? One sentence."
COPY_PROMPT=" What does this script do? One sentence."
AGENT_TIMEOUT="${SOT_MEDIA_AGENT_TIMEOUT:-420}"
# Flag settings for the demo session: no status line (the operator's shows
# session cost and account details), no spinner tips and one plain spinner
# verb ("Working", not a rotating list), no suggested next prompt (it
# would stand greyed in the input line of every end frame), no skills synced from
# the operator's claude.ai account and none bundled with Claude Code (only
# the project's julia-repl and show-result), and the full-screen renderer, which
# positions every cell itself instead of redrawing by counted lines. The
# agent may edit the demo project with its own Edit and Write tools.
AGENT_SETTINGS='{"statusLine":{"type":"command","command":"true"},"tui":"fullscreen","spinnerTipsEnabled":false,"promptSuggestionEnabled":false,"spinnerVerbs":{"mode":"replace","verbs":["Working"]},"syncClaudeAiSkills":false,"disableBundledSkills":true,"permissions":{"allow":["Bash(sot-fe:*)","Bash(show-result:*)","Edit(//home/demo/DemoProject/**)","Write(//home/demo/DemoProject/**)"]}}'
# The show-result skill's example reply says "nav pane"; the demo's layout
# shows a result in the preview, so the system prompt names it.
AGENT_SYSTEM="Edit files with your Edit and Write tools, never with sed, python or a shell redirect. A result you show opens in the preview pane, beside the file tree; call it the preview."
# From README.md (where a row switch leaves the cursor): expand src/,
# scripts/ and data/, then down to scripts/route.jl.
COPY_WALK="key Up;key Up;key Up;sleep 0.5;key Right;sleep 0.5;key Up;sleep 0.3;key Right;sleep 0.5;key Up;sleep 0.3;key Right;sleep 1
key Down;key Down;key Down;key Down;key Down;key Down;key Down;key Down;sleep 3"
# navigate: from data/'s first entry (the leg notes, open when the take
# starts) through the agent's bar chart, the HDF5 and the PDF to the route
# figure, held (Modules mode has its own loop).
NAV_KEYS="sleep 2.5
key Down;sleep 2.5;key Down;sleep 2.5;key Down;sleep 2.5;key Down;sleep 4.5"
row_raw() {    # row_raw <label> <bytes> — raw input to the row's pty, no Enter
    local b64; b64="$(printf '%s' "$2" | base64 -w0)"
    sotreq pty.input "$(jq -nc --arg w "${ROW_ID[$1]}" --arg d "$b64" '{workspace_id:$w,data_b64:$d,enter:false}')" \
        >>"$WORK/rows.log" 2>&1 || true
}
agent_transcript_dir() { printf '%s/.claude/projects/%s' "$HOME" "$(printf '%s' "$DEMO" | tr '/.' '--')"; }
since() { awk -v a="$1" -v b="$(date +%s.%N)" 'BEGIN{printf "%.1f", b-a}'; }
# agent_idle <text> <timeout> — until the transcript holding the prompt
# <text> ends on the agent's end_turn (its reply is complete), plus 2 s for
# the pane; 1 if it never does.
agent_idle() {
    local f i
    for i in $(seq 1 "$2"); do
        sleep 1
        f="$(grep -lF -- "$1" "$AGENT_TRANSCRIPT"/*.jsonl 2>/dev/null | xargs -r ls -t | sed -n 1p)"
        [ -n "$f" ] || continue
        if [ "$(jq -rs '[.[] | select(.type == "user" or .type == "assistant")] | last
                | if .type == "assistant" then .message.stop_reason // "" else "" end' "$f" 2>/dev/null)" = end_turn ]; then
            sleep 2; return 0
        fi
    done
    return 1
}
# Ctrl+L makes Claude Code redraw its screen (no stale cells of the /clear
# menu) before a recording starts.
redraw() { row_raw DemoProject $'\x0c'; sleep 1.5; }
# ask <take> <prompt> <text> — one follow-up take: /clear (local, no
# request) first so the pane holds only this exchange, then ASK_KEYS and the
# question, recorded until the answer is complete. T_ASK_<take> in times.sh
# is when the question is sent (agent_cut speeds up the wait after it).
ask() {
    local t0
    row_type DemoProject "/clear"; sleep 3; redraw; grab "$dbg/$1-cleared.png"
    rec_start "$AGENT_RAW/$1.mkv"; t0="$(date +%s.%N)"
    keys "${ASK_KEYS:-sleep 1.5}"
    row_type DemoProject "$2"
    printf 'T_ASK_%s=%s\n' "$1" "$(since "$t0")" >>"$AGENT_RAW/times.sh"
    sleep 5; agent_idle "$3" 180 || echo "agent take $1: no end of turn within 180 s" >&2
    rec_stop; grab "$dbg/$1-end.png"
}
clip_agent() {     # -> $AGENT_RAW/{hero,navigate,repl,crop,copy}.mkv, layout.png + times.sh
    local fig="$HOSTHOME/DemoProject/data/legs.png" t0 i T_PROMPT T_FIG=""
    dbg="$REPO/dev/output/agent-debug"   # gitignored: pane snapshots for a failed take
    AGENT_TRANSCRIPT="$(agent_transcript_dir)"
    rm -f "${fig:?}"; rm -rf "${dbg:?}" "${AGENT_RAW:?}"; mkdir -p "$dbg" "$AGENT_RAW"
    # The operator's own instructions (their user CLAUDE.md, auto-memory)
    # stay out of the demo session: only the product's installed skills apply.
    jq -c --arg md "$HOME/.claude/CLAUDE.md" '. + {claudeMdExcludes: [$md], autoMemoryEnabled: false}' \
        <<<"$AGENT_SETTINGS" >"$HOSTHOME/claude-demo-settings.json"
    # The session reads no user settings (--setting-sources project), so the
    # product's own skills go where a project's skills live.
    mkdir -p "$HOSTHOME/DemoProject/.claude/skills"
    cp -r "$REPO/comm/adapters/claude/julia-repl" "$REPO/comm/adapters/claude/show-result" \
        "$HOSTHOME/DemoProject/.claude/skills/"
    rows_state analysis:working figures:waiting gpu-train:idle survey:idle
    write_layout "$AGENT_WIDTHS" "$AGENT_COLUMNS"
    FE_H=$HERO_H start_fe --start-mode files
    sleep 10; attach_row
    # The pane's pty reports one column more than the pane shows (a
    # full-screen TUI would wrap each line's last cell), so the shell narrows
    # it by one before starting Claude Code.
    row_type DemoProject "clear; stty cols \$((\$(stty size | cut -d' ' -f2) - 1)); HOME=$HOME PATH=$HOME/.local/bin:\$PATH CLAUDE_CODE_NO_FLICKER=1 SOT_COMM_HOOKS=off SOT_COMM_HOME=$DHOME/.sot-comm SOT_WORKSPACE_ID=${ROW_ID[DemoProject]} $HOME/.local/bin/claude --permission-mode auto --setting-sources project --settings $DHOME/claude-demo-settings.json --append-system-prompt '$AGENT_SYSTEM'"
    sleep 15; grab "$dbg/a-launched.png"
    # The first-run folder-trust prompt: its default is "No, exit"; Down selects Yes.
    row_raw DemoProject $'\e[B'; sleep 0.8; row_raw DemoProject $'\r'
    # The previews and the REPL warm while Claude Code starts.
    warm; grab "$dbg/b-ready.png"
    # /clear (local, no request); the welcome banner, which names the
    # account's plan, stays at the top until the conversation scrolls it
    # away, and agent_cut opens the hero only once it is gone.
    # scripts/route.jl in the preview, the Julia drawer open.
    row_type DemoProject "/clear"; sleep 3
    keys "key Right;sleep 0.6;key Down;sleep 1;$DRAWER_OPEN;sleep 2"; redraw; grab "$dbg/b-cleared.png"
    rec_start "$AGENT_RAW/hero.mkv"; t0="$(date +%s.%N)"
    keys "sleep 2"
    row_type DemoProject "$AGENT_PROMPT"
    T_PROMPT="$(since "$t0")"
    sleep 4; rows_state survey:working
    for i in $(seq 1 "$AGENT_TIMEOUT"); do
        sleep 1
        [ "$i" = 60 ] && rows_state figures:working gpu-train:blocked
        [ $((i % 30)) = 0 ] && grab "$dbg/c-t$(printf %03d "$i").png"
        if [ -s "$fig" ]; then T_FIG="$(since "$t0")"; break; fi
    done
    [ -n "$T_FIG" ] || { rec_stop; cp "$WORK/sot.log" "$WORK/sotd.log" "$WORK/rows.log" "$dbg/" 2>/dev/null
        die "agent take: no $fig after ${AGENT_TIMEOUT}s (snapshots in $dbg)"; }
    # The agent's REPL run, the figure in the preview, its closing summary:
    # the take ends once the agent is idle (the true end frame).
    agent_idle "bar chart of the distance" 240 || echo "agent take hero: no end of turn within 240 s" >&2
    rows_state analysis:done survey:done; sleep 3
    rec_stop
    printf 'T_PROMPT=%s\nT_FIG=%s\n' "$T_PROMPT" "$T_FIG" >"$AGENT_RAW/times.sh"
    # navigate: the finished conversation stays in the agent pane while the
    # user browses. layout.png (the labelled layout's source) is this
    # layout once the take ends, back on the agent's bar chart, the drawer
    # opened and a run of the script in it.
    stop_fe
    write_layout "$NAV_WIDTHS" "$NAV_COLUMNS"
    start_fe --start-mode files
    sleep 10; attach_row
    keys "key s;sleep 1;key f;sleep 1;$DATA;sleep 6"; redraw   # the mode round trip leaves the plain status
    rec_start "$AGENT_RAW/navigate.mkv"; keys "$NAV_KEYS"; rec_stop
    keys "key Up;key Up;key Up;sleep 4;key ctrl+j;sleep 2;type include(\"scripts/route.jl\");key Return;sleep 10;key Escape;sleep 2"
    grab "$AGENT_RAW/layout.png"
    stop_fe
    # repl: the drawer cleared, the agent asked to run the script; the
    # drawer shows its run.
    write_layout "$AGENT_WIDTHS" "$AGENT_COLUMNS"
    start_fe --start-mode files
    sleep 10; attach_row
    keys "$SCRIPT;sleep 1;key ctrl+j;sleep 2;key ctrl+l;sleep 0.5;key Escape;sleep 5"
    ask repl "$REPL_PROMPT" "$REPL_PROMPT"
    # crop: the drawer closed, the route figure zoomed twice on its middle
    # and panned right once (the short Black Mesa - Santa Fe leg, no axis
    # in view), c sends the visible region to the agent's input, and
    # the agent answers. The figure is put in the preview with the product's
    # own `sot-fe preview` (what show-result sends), not a tree walk.
    keys "key Escape;sleep 0.5;key ctrl+j;sleep 1"
    "${DEMO_ENV[@]}" "$HOME/.local/bin/sot-fe" preview "${ROW_SLUG[DemoProject]}" data/track.png >>"$WORK/rows.log" 2>&1 || true
    sleep 3
    ASK_KEYS="sleep 1.5;key ctrl+Right;sleep 1;key =;sleep 1.5;key =;sleep 1.5;key Right;sleep 2;key c;sleep 2" \
        ask crop "$CROP_PROMPT" "part of the figure is the shortest"
    # copy: c copies route.jl's path in the Files pane, Ctrl+V pastes it in
    # the agent pane, the agent answers about it. Focus still steps
    # nav -> preview -> llm with no preview column, so Ctrl+Right twice.
    stop_fe
    write_layout "$AGENT_WIDTHS" "$COPY_COLUMNS" "$COPY_DRAWER"
    start_fe --start-mode files
    sleep 10; attach_row
    keys "$COPY_WALK;$DRAWER_OPEN;sleep 2"
    ASK_KEYS="sleep 1.5;key c;sleep 1.5;key ctrl+Right;sleep 0.3;key ctrl+Right;sleep 1;key ctrl+v;sleep 1.5" \
        ask copy "$COPY_PROMPT" "What does this script do"
    row_raw DemoProject $'\x03'; sleep 1; row_raw DemoProject $'\x03'; sleep 2
    stop_fe
    write_layout "$WIDTHS"
}
# banner_masks <take> — ffmpeg drawbox filters that blank, in every frame
# where the /clear line shows, the agent pane from its top row down through
# it: Claude Code's banner (the orange logo beside the version, the model
# and the account's plan), the promo line under it and the /clear echo
# all sit there, and scroll off before the /clear line does; "null" if no
# frame shows it.
# It is found per frame as the topmost pixel row whose text ends where
# "/clear" ends (x 92..108 past the border; the empty input line ends
# sooner, any other line later, "Working…" included), with a dark gap
# before the text (not the logo) and the grey prompt mark in the gutter
# within 8 px (not a white or green tool bullet).
banner_masks() {
    ffmpeg -loglevel error -i "$1" -vf "fps=$FPS,crop=200:480:$((AGENT_PANE_X + 2)):40" \
        -f rawvideo -pix_fmt rgb24 - | python3 -c '
import sys
fps, x0, w, bg = float(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
C, R = 200, 480
lit = bytes(1 if v > 90 else 0 for v in range(256))
def grey(r):
    return any(60 < max(p) < 180 and max(p) - min(p) < 25 for p in zip(r[0:60:3], r[1:60:3], r[2:60:3]))
bots = []
while (f := sys.stdin.buffer.read(3 * C * R)) and len(f) == 3 * C * R:
    rows = [f[3 * C * y:3 * C * (y + 1)] for y in range(R)]
    g = [r[1::3] for r in rows]
    end = [r[22:].translate(lit).rfind(1) + 22 for r in g]
    ys = [y for y in range(R) if 92 <= end[y] <= 108 and max(g[y][14:26]) < 60
          and any(grey(r) for r in rows[max(0, y - 8):y + 9])]
    y = ys[0] if ys else -1
    while y >= 0 and y + 1 < R and (y + 1) in ys: y += 1
    bots.append(40 + y + 14 if y >= 0 else -1)
out, i = [], 0
while i < len(bots):
    j = i
    while j + 1 < len(bots) and bots[j + 1] == bots[i]: j += 1
    if bots[i] > 0:
        out.append("drawbox=x=%d:y=40:w=%d:h=%d:color=%s:t=fill:enable=%sbetween(t,%.3f,%.3f)%s"
                   % (x0, w, bots[i] - 40, bg, chr(39), max(0, (i - 1) / fps), (j + 2) / fps, chr(39)))
    i = j + 1
print(",".join(out) or "null")' "$FPS" "$((AGENT_PANE_X + 2))" 1020 "$PANE_BG"
}
# Crops of the agent takes (WxH+X+Y). The agent pane (x 856..1920) is
# ~79 columns wide, so no crop can narrow the window with the panes side by
# side. A follow-up take starts after /clear, which leaves the banner (it
# names the account's plan) at the agent pane's top: the gallery crops'
# agent rows start below the banner and the /clear line, on a row boundary
# (rows are 27 px apart), and hold whole rows. The full-window loops blank
# the banner (banner_masks).
AGENT_PANE_X=858                     # the agent pane's left border
# The hero window is HERO_H tall (FE_H), so the agent's rows, the preview
# and the drawer's chart fill it; HERO_CROP drops its bottom border (the
# fe/be version label) and the session strip.
HERO_H=860
HERO_CROP="1920x800+0+0"
# The agent rows are cropped inside the pane's borders (x 862..1886).
REPL_TOP="840x536+16+476";  REPL_AGENT="1024x298+862+257"    # the drawer
CROP_TOP="840x640+16+210";  CROP_AGENT="1024x324+862+257"    # the zoomed figure
COPY_TOP="700x486+16+16";   COPY_AGENT="1024x270+862+257"    # the Files pane, its header in
NAVIGATE_G="840x$BODY_H+16+16"                               # navigate-g, navigate-phone: nav and preview
# repl: the window without its top row (the preview's title names the
# figure's scratch path under .sot/runs) or its bottom row (the fe/be
# version label and the session strip).
REPL_FULL="1886x976+17+40"
GIF_LEFT="760x784+26+16"; GIF_AGENT="1016x784+862+16"          # hero.gif: the left column beside the agent's rows
PHONE_CHART="760x330+28+42"; PHONE_AGENT="1016x244+862+230"   # hero-phone.png: the chart over the agent's last rows
PANE_BG=0x050917                     # the agent pane's background (banner_masks)
# The agent pane's title row ends in a clock (the recording's date and time):
# blanked, and the title rule under it redrawn (y 30-31, from the gap before
# the clock up to the corner). The pane's right border (x 1888-1889) has
# breaks the frontend leaves in it at some rows (y 369-395 in these takes);
# it is redrawn whole.
CLOCK_MASK="drawbox=x=1612:y=16:w=274:h=26:color=$PANE_BG:t=fill,drawbox=x=1612:y=30:w=276:h=2:color=0x666666:t=fill,drawbox=x=1888:y=30:w=2:h=1000:color=0x666666:t=fill"
# One text scale: every gallery file keeps the window's own pixels (a
# terminal cell 27 px tall at font scale 1.5), cropped tight to the region
# it is about, and the site sizes each file from its own width (width x
# 0.54), so text is one size on every page. A file that shows two regions
# holds them as framed cards: a 1 px CARD_BORDER, GAP px of PAGE_BG around
# and between.
PAGE_BG=0x1b1b1f; CARD_BORDER=0x3c3f44; GAP=16
cards_vf() {   # cards_vf v|h <WxH+X+Y>... — the regions as framed cards, stacked (v) or side by side (h)
    local dir="$1" n i=0 g w h x y mw=0 mh=0 sw=0 f="" lab="" pw ph px py tw th
    shift; n=$#
    for g in "$@"; do IFS='x+' read -r w h x y <<<"$g"; ((w > mw)) && mw=$w; ((h > mh)) && mh=$h; sw=$((sw + w + 2 + GAP)); done
    f="split=$n$(for i in $(seq 1 "$n"); do printf '[s%d]' "$i"; done)"; i=0
    for g in "$@"; do
        i=$((i + 1)); IFS='x+' read -r w h x y <<<"$g"
        if [ "$dir" = v ]; then pw=$((mw + 2)); ph=$((h + 2 + GAP)); px="(ow-iw)/2"; py=$GAP
        else pw=$((w + 2 + GAP)); ph=$((mh + 2)); px=$GAP; py=0; fi
        f+=";[s$i]crop=$w:$h:$x:$y,pad=$((w + 2)):$((h + 2)):1:1:color=$CARD_BORDER,pad=$pw:$ph:$px:$py:color=$PAGE_BG[c$i]"
        lab+="[c$i]"
    done
    if [ "$dir" = v ]; then tw=$((mw + 2 + 2 * GAP)); th="ih+$GAP"; py=0
    else tw=$((sw + GAP)); th="ih+$((2 * GAP))"; py=$GAP; fi
    local stack="${dir}stack=inputs=$n"; ((n > 1)) || stack=null
    echo "$f;${lab}$stack,pad=$tw:$th:(ow-iw)/2:$py:color=$PAGE_BG"
}
region_vf() {  # region_vf <WxH+X+Y>... — one region a plain crop, two or more stacked cards
    local w h x y
    [ $# -gt 1 ] && { cards_vf v "$@"; return; }
    IFS='x+' read -r w h x y <<<"$1"; echo "crop=$w:$h:$x:$y"
}
# The hero cut, from the take's own times: it opens HERO_OPEN_AT seconds
# after the prompt (the request on screen) and plays HERO_OPEN seconds at
# real speed; the agent's work up to 1.5 s before the figure file appears
# is sped up to HERO_MID seconds; the next HERO_REAL seconds (the REPL run)
# play at real speed; the rest (the figure in the preview, the agent's
# summary, up to the agent idle) is sped up to HERO_TAIL seconds but at
# least HERO_TAIL_K times, up to T_END when times.sh sets one; the end frame
# is held HERO_HOLD seconds and is the poster and agent-result.png.
HERO_OPEN_AT=0.5; HERO_OPEN=2.5; HERO_MID=4; HERO_REAL=2; HERO_TAIL=6; HERO_TAIL_K=3; HERO_HOLD=3.5
ASK_OPEN=1.5; ASK_SHOW=2.5; ASK_K=4; ASK_HOLD=2; LEAD_K=3
agent_cut() {      # $AGENT_RAW -> the agent loops, hero.gif, hero-phone.png, agent-result.png, layout-labelled.png
    local T_PROMPT T_FIG T_END="" s o f1 f2 k kt len hl t mask skip ask p a b
    [ -s "$AGENT_RAW/times.sh" ] || die "no agent takes in $AGENT_RAW (run: docs-media.sh agent)"
    # shellcheck disable=SC1091
    . "$AGENT_RAW/times.sh"
    len="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$AGENT_RAW/hero.mkv")"
    s="$(awk -v p="$T_PROMPT" -v a="$HERO_OPEN_AT" 'BEGIN{printf "%.2f", p+a}')"
    o="$(awk -v s="$s" -v r="$HERO_OPEN" 'BEGIN{printf "%.2f", s+r}')"
    f1="$(awk -v f="$T_FIG" 'BEGIN{printf "%.2f", f-1.5}')"
    f2="$(awk -v f="$f1" -v r="$HERO_REAL" 'BEGIN{printf "%.2f", f+r}')"
    local tend="${T_END:-$len}"
    mask="$(banner_masks "$AGENT_RAW/hero.mkv")"
    # Until the figure exists the preview shows scripts/route.jl, whose last
    # visible row the frontend draws cut in half above the drawer (y 345-358):
    # blanked for that part of the take.
    k="$(awk -v a="$o" -v b="$f1" -v m="$HERO_MID" 'BEGIN{k=(b-a)/m; if (k<1) k=1; printf "%.2f", k}')"
    kt="$(awk -v a="$f2" -v b="$tend" -v m="$HERO_TAIL" -v k="$HERO_TAIL_K" 'BEGIN{x=(b-a)/m; if (x<k) x=k; printf "%.2f", x}')"
    ffmpeg -loglevel error -y -i "$AGENT_RAW/hero.mkv" -filter_complex \
        "[0:v]$mask,$CLOCK_MASK,drawbox=x=26:y=345:w=828:h=14:color=$PANE_BG:t=fill:enable='lt(t,$T_FIG)',crop=${HERO_CROP//[x+]/:},split=4[a][b][c][d];[a]trim=$s:$o,setpts=PTS-STARTPTS[w];[b]trim=$o:$f1,setpts=(PTS-STARTPTS)/$k[x];[c]trim=$f1:$f2,setpts=PTS-STARTPTS[y];[d]trim=$f2:$tend,setpts=(PTS-STARTPTS)/$kt[z];[w][x][y][z]concat=n=4:v=1,fps=$FPS,tpad=stop_mode=clone:stop_duration=$HERO_HOLD[v]" \
        -map "[v]" -c:v libx264rgb -crf 0 -preset ultrafast "$WORK/hero.mkv"
    hl="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$WORK/hero.mkv")"
    printf 'take %.0fs, cut to %.0fs: opens %ss in, on the typed request; %ss-%ss real speed; %ss-%ss (the agent working) sped up %sx; %ss-%ss real speed; %ss-%.0fs sped up %sx; last frame held %ss; the banner rows blanked while they show\n' \
        "$len" "$hl" "$s" "$s" "$o" "$o" "$f1" "$k" "$f1" "$f2" "$f2" "$tend" "$kt" "$HERO_HOLD" >"$OUT/hero.speed.txt"
    encode "$WORK/hero.mkv" hero "$(awk -v l="$hl" 'BEGIN{printf "%.2f", l-0.2}')"
    cp "$OUT/hero.png" "$OUT/agent-result.png"
    ffmpeg -loglevel error -y -i "$OUT/hero.png" -vf "$(cards_vf v "$PHONE_CHART" "$PHONE_AGENT")" "$OUT/hero-phone.png"
    # README fallback: the left column beside the agent's rows as two cards,
    # tighter than the hero for GitHub's ~830 px column; flat colours, no
    # dither (dithered bars speckle).
    ffmpeg -loglevel error -y -i "$WORK/hero.mkv" -filter_complex \
        "fps=8,$(cards_vf h "$GIF_LEFT" "$GIF_AGENT"),split[p][q];[p]palettegen=max_colors=256:stats_mode=full[pal];[q][pal]paletteuse=dither=none:diff_mode=rectangle" \
        "$OUT/hero.gif"
    # The other takes: the banner rows blanked (a follow-up take starts
    # under the banner), REPL_SKIP cut from the repl take; in a follow-up
    # the lead-in (up to 1 s before its question is sent) sped up LEAD_K
    # times, the wait for the answer (ASK_OPEN s after the question, up to
    # ASK_SHOW s before the end) ASK_K times, and the last frame held
    # ASK_HOLD s, so the loop spends its time on the answer; the poster is
    # the held result.
    for t in navigate repl crop copy; do
        len="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$AGENT_RAW/$t.mkv")"
        mask="$(banner_masks "$AGENT_RAW/$t.mkv")"
        skip=0
        if [ "$t" = repl ] && [ -n "${REPL_SKIP:-}" ]; then
            mask+=",select='not(between(t,${REPL_SKIP%:*},${REPL_SKIP#*:}))',setpts=N/$FPS/TB"
            skip="$(awk -v a="${REPL_SKIP%:*}" -v b="${REPL_SKIP#*:}" 'BEGIN{print b-a}')"
        fi
        ask="T_ASK_$t"; ask="${!ask:-}"
        if [ -n "$ask" ]; then
            read -r p a b len < <(awk -v q="$ask" -v l="$len" -v s="$skip" -v o="$ASK_OPEN" -v e="$ASK_SHOW" -v k="$ASK_K" -v lk="$LEAD_K" -v h="$ASK_HOLD" \
                'BEGIN{p=q-1; if (p<0) p=0; a=q+o; b=l-s-e; printf "%.2f %.2f %.2f %.2f\n", p, a, b, p/lk+(a-p)+(b-a)/k+e+h}')
            local pa="$p/$LEAD_K+T-$p" ab="$p/$LEAD_K+$a-$p+(T-$a)/$ASK_K" bz="$p/$LEAD_K+$a-$p+($b-$a)/$ASK_K+T-$b"
            mask+=",setpts='if(lt(T,$p),T/$LEAD_K,if(lt(T,$a),$pa,if(lt(T,$b),$ab,$bz)))/TB',fps=$FPS,tpad=stop_mode=clone:stop_duration=$ASK_HOLD"
        fi
        len="$(awk -v l="$len" 'BEGIN{printf "%.1f", l-1}')"
        case "$t" in
            # navigate-g's poster is the HDF5 preview (6.5 s), which fills the pane.
            navigate) encode "$AGENT_RAW/$t.mkv" navigate-g 6.5 "$mask,$(region_vf "$NAVIGATE_G")"
                      cp "$OUT/navigate-g.png" "$OUT/navigate-g-phone.png" ;;
            repl)     encode "$AGENT_RAW/$t.mkv" repl "$len" "$mask,$(region_vf "$REPL_FULL")"
                      encode "$AGENT_RAW/$t.mkv" repl-g "$len" "$mask,$(region_vf "$REPL_TOP" "$REPL_AGENT")" ;;
            crop)     encode "$AGENT_RAW/$t.mkv" crop "$len" "$mask,$(region_vf "$CROP_TOP" "$CROP_AGENT")" ;;
            copy)     encode "$AGENT_RAW/$t.mkv" copy "$len" "$mask,$(region_vf "$COPY_TOP" "$COPY_AGENT")" ;;
        esac
    done
    ffmpeg -loglevel error -y -i "$AGENT_RAW/layout.png" -vf "$CLOCK_MASK" "$WORK/layout.png"
    "$REPO/docs/src/assets/readme/make-layout.sh" "$WORK/layout.png"
    say "agent cut: $(cat "$OUT/hero.speed.txt")"
}

optimize() {
    if command -v oxipng >/dev/null; then
        oxipng -q -o 4 --strip safe "$OUT"/*.png
    else
        say "oxipng not found — PNGs left unoptimized"
    fi
}

cmd="${1:-}"; [ $# -gt 0 ] && shift
case "$cmd" in
    list)   list_matrix ;;
    stills) prepare; run_stills "$@"; optimize ;;
    loops)  prepare; run_loops "$@"; optimize ;;
    features) prepare; run_features "$@"; optimize ;;
    all)    prepare; run_stills; run_loops; run_features; optimize ;;
    agent)  W=$AGENT_W; DAEMON_RW="$AGENT_RW"; prepare; clip_agent; say "takes kept in $AGENT_RAW; next: docs-media.sh agent-cut" ;;
    agent-cut) agent_cut; optimize ;;
    *)      die "usage: docs-media.sh stills [name...] | loops [name...] | features [name...] | all | agent | agent-cut | list" ;;
esac
say "done -> $OUT"
