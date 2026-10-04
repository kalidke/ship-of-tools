# shellcheck shell=bash
# docs-media-stage.sh -- the demo stage for docs-media.sh: environment, demo rows, scratch daemon and frontend,
# screen recording. Sourced by docs-media.sh where these lines were.

# ---- environment -------------------------------------------------------------
WIDTHS="0.31,0.35,0.34"   # nav, preview, llm: the default layout (see prepare)
write_layout() {  # write_layout <widths> [columns] [drawer height] — the demo user's settings.toml, read at FE start
    mkdir -p "$HOSTHOME/.config/sot"
    printf '[layout]\npreset = "laptop"\n\n[layout.laptop]\ncolumns = "%s"\nwidths = "%s"\ndrawer = "repl"\ndrawer_height = "%s"\n' \
        "${2:-nav,preview,llm}" "$1" "${3:-0.52}" >"$HOSTHOME/.config/sot/settings.toml"
}
prepare() {
    command -v Xvfb >/dev/null || die "Xvfb not found"
    command -v ffmpeg >/dev/null || die "ffmpeg not found"
    command -v python3 >/dev/null || die "python3 not found"
    command -v jq >/dev/null || die "jq not found"
    [ -x "$BIN/sot" ] && [ -x "$BIN/sotd" ] || die "no sot/sotd in $BIN (set SOT_MEDIA_BIN)"
    case "$HOME" in /home/?*) ;; *) die "the /home overlay assumes HOME under /home (got $HOME)" ;; esac
    local icd=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
    [ -f "$icd" ] || die "lavapipe ICD not found ($icd) — install mesa-vulkan-drivers"
    mkdir -p "$OUT" "$WORK"/{lib,shim,config/sot,state,runtime,cache,realhome} "$HOSTHOME/.sot-comm/bin"
    chmod 700 "$WORK/runtime"

    # winit's X11 backend dlopens libxkbcommon-x11 + libxcb-xkb, which this
    # distro may lack; the Julia artifact store carries both. libxkbcommon-x11
    # must load with the libxkbcommon from its OWN artifact: pairing a newer
    # x11 lib with the distro's older libxkbcommon segfaults the frontend.
    local depot="${JULIA_DEPOT_PATH%%:*}" f x11dir
    x11dir="$(dirname "$(find "$depot/artifacts" -name 'libxkbcommon-x11.so.0' 2>/dev/null | head -1)")"
    [ -e "$x11dir/libxkbcommon-x11.so.0" ] || die "no libxkbcommon-x11 in $depot/artifacts"
    for f in "$x11dir"/libxkbcommon.so* "$x11dir"/libxkbcommon-x11.so* \
             $(find "$depot/artifacts" -name 'libxcb-xkb.so.1' 2>/dev/null | head -1); do
        [ -e "$WORK/lib/$(basename "$f")" ] || ln -s "$f" "$WORK/lib/"
    done
    # No systemd user manager for the demo: a capsule supervisor takes the
    # contained spawn path, never a scope on the invoking user's manager.
    printf '#!/bin/sh\nexit 1\n' >"$WORK/shim/systemd-run"; chmod +x "$WORK/shim/systemd-run"

    # The demo home: the project, one plain directory per extra session row,
    # the comm scripts where an install puts them, and a prompt that names
    # no real user.
    cp -r "$FIXTURE" "$HOSTHOME/DemoProject"
    mkdir -p "$HOSTHOME"/{analysis,figures,gpu-train,survey}
    cp "$REPO/comm/core/scripts/"* "$HOSTHOME/.sot-comm/bin/"
    printf "PS1='demo@\\\\h:\\\\w\\\\$ '\n" >"$HOSTHOME/.bashrc"
    # Pasted text (the crop and copy loops) stays plain, not bash's
    # reverse-video paste highlight (readline 8.1 has no switch for the
    # highlight alone; the pasted bytes still arrive intact).
    printf 'set enable-bracketed-paste off\n' >"$HOSTHOME/.inputrc"
    # The demo user's layout: the llm column holds only a shell, so nav and
    # preview get its width (the nav status line and the fixture's lines fit
    # unwrapped) and the Julia drawer is tall enough for a legible figure.
    # nav + preview = 0.66 of 1920 px: the llm border sits at x ~1267, where
    # the sessions loop is cropped.
    write_layout "$WIDTHS"
    prepare_data
    local hash; hash="$(sha256sum "$HOSTHOME/DemoProject/src/DemoProject.jl" | cut -d' ' -f1)"
    sed -i "s/^synced_against: .*/synced_against: \"$hash\"/" "$HOSTHOME/DemoProject/.concept/modules/DemoProject.md"

    cat >"$WORK/hostwrap.py" <<'PY'
# hostwrap.py <demo-home-src> cmd... — new user + UTS + mount namespace
# mapping only our own uid/gid, hostname "demo", /home replaced by a tmpfs
# holding /home/demo (bound from <demo-home-src>) and our own home read-only
# (so the julia and sot binaries still resolve, and nothing writes it), then
# exec. Done in-process: the
# namespace's capabilities die at execve for a non-root uid, so `unshare(1)`
# plus separate mount/sethostname calls cannot work.
import ctypes, os, pwd, socket, sys
libc = ctypes.CDLL(None, use_errno=True)
def check(rc, what):
    if rc != 0: sys.exit("hostwrap: %s: %s" % (what, os.strerror(ctypes.get_errno())))
def mount(src, dst, fstype, flags, data=None):
    check(libc.mount(src and src.encode(), dst.encode(), fstype and fstype.encode(),
                     ctypes.c_ulong(flags), data and data.encode()), "mount " + dst)
MS_RDONLY, MS_REMOUNT, MS_BIND, MS_REC, MS_PRIVATE = 1, 32, 0x1000, 0x4000, 1 << 18
demo_src, argv = sys.argv[1], sys.argv[2:]
uid, gid = os.getuid(), os.getgid()
real_home = pwd.getpwuid(uid).pw_dir
stash = os.path.join(os.path.dirname(demo_src), "realhome")
check(libc.unshare(0x10000000 | 0x04000000 | 0x00020000), "unshare")  # NEWUSER|NEWUTS|NEWNS
for path, text in (("uid_map", f"{uid} {uid} 1"), ("setgroups", "deny"), ("gid_map", f"{gid} {gid} 1")):
    with open("/proc/self/" + path, "w") as f: f.write(text)
socket.sethostname("demo")
mount(None, "/", None, MS_REC | MS_PRIVATE)
mount(real_home, stash, None, MS_BIND | MS_REC)
mount("tmpfs", "/home", "tmpfs", 0, "mode=755")
for d, src in ((real_home, stash), ("/home/demo", demo_src)):
    os.makedirs(d)
    mount(src, d, None, MS_BIND | MS_REC)
# Read-only remount; a bind remount must restate the mount's locked flags.
st = os.statvfs(real_home).f_flag
keep = sum(ms for stf, ms in ((2, 2), (4, 4), (8, 8), (1024, 1024), (2048, 2048), (4096, 1 << 21)) if st & stf)
mount(None, real_home, None, MS_REMOUNT | MS_BIND | MS_RDONLY | keep)
# File owners show as "demo", never the invoking user's name.
for f in ("passwd", "group"):
    fake = os.path.join(os.path.dirname(demo_src), f)
    if os.path.exists(fake): mount(fake, "/etc/" + f, None, MS_BIND)
# The agent step only: the paths the operator's own Claude Code writes
# (its config, caches, state), bound read-write back over the read-only home.
for rel in filter(None, os.environ.get("HOSTWRAP_RW", "").split(":")):
    src, dst = os.path.join(stash, rel), os.path.join(real_home, rel)
    if os.path.exists(src): mount(src, dst, None, MS_BIND)
os.chdir(os.environ.get("HOSTWRAP_CWD", "/"))
os.execvp(argv[0], argv)
PY
    write_xdrive
    # /etc/passwd and /etc/group for the namespace: the invoking uid/gid
    # renamed demo, home /home/demo (hostwrap binds them over /etc).
    local me grp; me="$(id -un)"; grp="$(id -gn)"
    awk -F: -v OFS=: -v u="$(id -u)" '$3 == u { $1 = "demo"; $5 = "demo"; $6 = "/home/demo" } { print }' /etc/passwd >"$WORK/passwd"
    awk -F: -v OFS=: -v g="$(id -g)" '$3 == g { $1 = "demo" } { print }' /etc/group | sed "s/\b$me\b/demo/g" >"$WORK/group"

    # A free display number: no socket and no lock file.
    local n
    for n in $(seq 140 199); do
        [ -e "/tmp/.X11-unix/X$n" ] || [ -e "/tmp/.X$n-lock" ] || { DISPLAY_NUM=":$n"; break; }
    done
    [ -n "${DISPLAY_NUM:-}" ] || die "no free X display in :140-:199"
    spawn "$WORK/xvfb.log" Xvfb "$DISPLAY_NUM" -screen 0 "${W}x${H}x24" -nolisten tcp
    for _ in $(seq 1 50); do [ -e "/tmp/.X11-unix/X${DISPLAY_NUM#:}" ] && break; sleep 0.1; done
    [ -e "/tmp/.X11-unix/X${DISPLAY_NUM#:}" ] || die "Xvfb did not start (see $WORK/xvfb.log)"
    # A desktop's clipboard keeper: the frontend's clipboard handle is
    # short-lived, so without one a copied path is gone before the paste.
    # Mapped before the frontend, so it stays hidden beneath it.
    if command -v xclipboard >/dev/null; then
        spawn "$WORK/xclipboard.log" env DISPLAY="$DISPLAY_NUM" xclipboard -geometry 1x1+0+0
    fi

    DEMO_ENV=(python3 "$WORK/hostwrap.py" "$HOSTHOME"
        env -i
        PATH="$WORK/shim:$DHOME/.sot-comm/bin:$(dirname "$(command -v julia)"):/usr/local/bin:/usr/bin:/bin"
        HOME="$DHOME" SHELL=/bin/bash HOSTNAME=demo USER=demo LOGNAME=demo LANG=C.UTF-8 TERM=xterm-256color
        JULIA_DEPOT_PATH="$JULIA_DEPOT_PATH"
        XDG_CONFIG_HOME="$WORK/config" XDG_STATE_HOME="$WORK/state"
        XDG_RUNTIME_DIR="$WORK/runtime" XDG_CACHE_HOME="$WORK/cache"
        SOT_SOCKET="$WORK/runtime/sotd.sock"
        DISPLAY="$DISPLAY_NUM" LD_LIBRARY_PATH="$WORK/lib" VK_ICD_FILENAMES="$icd")

    # Precompile the demo env and draw the route figure once (data/track.png
    # is that script's own output), outside any recording window. A second
    # route drawn by the same plot_route gives data/ a same-size neighbour
    # (valley.png), which the zoom loop moves to.
    say "instantiating the demo env and running scripts/route.jl…"
    HOSTWRAP_CWD="$DEMO" "${DEMO_ENV[@]}" julia --project=. -e \
        'using Pkg; Pkg.instantiate(); include("scripts/route.jl")
         save("data/valley.png", plot_route([Waypoint("Albuquerque", 35.08, -106.65),
             Waypoint("Belen", 34.66, -106.78), Waypoint("Socorro", 34.06, -106.89),
             Waypoint("Truth or Consequences", 33.13, -107.25)]))' >"$WORK/instantiate.log" 2>&1 \
        || die "demo env failed (see $WORK/instantiate.log)"
    start_daemon
    start_rows
}

# data/: the notes are fixture files; the PDF and HDF5 come from
# examples/preview (the PDF rebuilt without its "preview sample" byline when
# pdflatex is present). Names sort in the order the navigate loop walks.
prepare_data() {
    local data="$HOSTHOME/DemoProject/data"
    cp "$REPO/examples/preview/sample.h5" "$data/samples.h5"
    if command -v pdflatex >/dev/null; then
        mkdir -p "$WORK/tex"
        # The sample's author line and its abstract's last sentence describe
        # it as a preview fixture; the demo's survey drops both.
        perl -0pe 's/\\author\{.*?\}\n/\\author{}\n/; s/ It doubles as a preview fixture,.*?table\.//s' \
            "$REPO/examples/preview/sample.tex" >"$WORK/tex/survey.tex"
        (cd "$WORK/tex" && pdflatex -interaction=batchmode survey.tex && pdflatex -interaction=batchmode survey.tex) \
            >"$WORK/tex/build.log" 2>&1 || die "pdflatex failed (see $WORK/tex/build.log)"
        cp "$WORK/tex/survey.pdf" "$data/survey.pdf"
    else
        cp "$REPO/examples/preview/sample.pdf" "$data/survey.pdf"
    fi
}

# The key driver: X11 XTest through ctypes (xdotool-equivalent, no packages).
# Input lines: "key <combo>" (ctrl+/shift+/alt+ prefixes, X keysym names),
# "type <text>", "sleep <seconds>".
write_xdrive() {
    cat >"$WORK/xdrive.py" <<'PY'
import ctypes as C, sys, time
x = C.CDLL("libX11.so.6"); t = C.CDLL("libXtst.so.6")
x.XOpenDisplay.restype = C.c_void_p; x.XOpenDisplay.argtypes = [C.c_char_p]
x.XStringToKeysym.restype = C.c_ulong; x.XStringToKeysym.argtypes = [C.c_char_p]
x.XKeysymToKeycode.restype = C.c_ubyte; x.XKeysymToKeycode.argtypes = [C.c_void_p, C.c_ulong]
x.XKeycodeToKeysym.restype = C.c_ulong; x.XKeycodeToKeysym.argtypes = [C.c_void_p, C.c_ubyte, C.c_int]
x.XFlush.argtypes = [C.c_void_p]
t.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
t.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]
d = x.XOpenDisplay(None)
if not d: sys.exit("xdrive: cannot open display")
MOD = {"ctrl": "Control_L", "shift": "Shift_L", "alt": "Alt_L"}
SYM = {" ": "space", "(": "parenleft", ")": "parenright", '"': "quotedbl", ".": "period",
       "/": "slash", "_": "underscore", ",": "comma", "=": "equal", ":": "colon",
       "?": "question", "-": "minus", "+": "plus", "'": "apostrophe",
       "~": "asciitilde", "&": "ampersand", "*": "asterisk"}
def code(name):
    ks = x.XStringToKeysym(SYM.get(name, name).encode())
    kc = x.XKeysymToKeycode(d, ks)
    if not kc: sys.exit("xdrive: no keycode for %r" % name)
    return kc, x.XKeycodeToKeysym(d, kc, 0) != ks      # (keycode, needs shift)
def ev(kc, down): t.XTestFakeKeyEvent(d, kc, down, 0); x.XFlush(d)
def combo(spec):
    parts = spec.split("+") if len(spec) > 1 else [spec]
    mods = [code(MOD[m])[0] for m in parts[:-1]]
    kc, shift = code(parts[-1])
    if shift: mods.append(code("Shift_L")[0])
    for m in mods: ev(m, 1)
    ev(kc, 1); time.sleep(0.03); ev(kc, 0)
    for m in reversed(mods): ev(m, 0)
    time.sleep(0.05)
# No window manager: hand keyboard focus to the frontend's (only) top-level
# window explicitly, or winit never sees a FocusIn and drops the keys.
x.XDefaultRootWindow.restype = C.c_ulong; x.XDefaultRootWindow.argtypes = [C.c_void_p]
x.XQueryTree.argtypes = [C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong),
                         C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)]
x.XSetInputFocus.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_ulong]
r, p, kids, n = C.c_ulong(), C.c_ulong(), C.POINTER(C.c_ulong)(), C.c_uint()
x.XQueryTree(d, x.XDefaultRootWindow(d), C.byref(r), C.byref(p), C.byref(kids), C.byref(n))
if n.value: x.XSetInputFocus(d, kids[n.value - 1], 2, 0)   # topmost; RevertToParent
t.XTestFakeMotionEvent(d, -1, 1900, 1060, 0); x.XFlush(d)
for line in sys.stdin.read().replace(";", "\n").splitlines():
    op, _, arg = line.strip().partition(" ")
    if not op or op.startswith("#"): continue
    if op == "key": combo(arg)
    elif op == "type":
        for ch in arg: combo(ch); time.sleep(0.04)
    elif op == "sleep": time.sleep(float(arg))
    else: sys.exit("xdrive: bad op %r" % op)
PY
}
keys() { printf '%s\n' "$*" | DISPLAY="$DISPLAY_NUM" python3 "$WORK/xdrive.py"; }
steps() {      # steps "a;b;..." — xdrive lines, plus "rows_state <spec>" run here
    local line batch=""
    while IFS= read -r line; do
        case "$line" in
            rows_state\ *) [ -n "$batch" ] && keys "$batch"; batch=""; rows_state ${line#rows_state } ;;
            *) batch+="$line"$'\n' ;;
        esac
    done <<<"${1//;/$'\n'}"
    [ -n "$batch" ] && keys "$batch"
    return 0
}

# ---- demo sessions -------------------------------------------------------------
# Live rows: each is a bash capsule the scratch daemon starts (workspace.create
# with agent "none" + boot, the op the frontend's own new-session flow sends).
# A row's work state is set the way an agent's hooks set it: the product's
# comm-join.sh / comm-status.sh typed into the row's own shell (pty.input, the
# op the daemon's wake uses), writing the throwaway HOME's registry. The first row
# is the project the frontend resumes into, so the llm column shows its shell.
#
# An agent segment would slot in here later: a row created with agent
# "claude"/"codex" instead of "none" (the daemon starts its launcher), with
# rows_state left to the agent's own hooks.
SESSION_ROWS=(DemoProject analysis figures gpu-train survey)
declare -A ROW_ID=() ROW_SLUG=()
sotreq() {     # sotreq <op> <payload-json> — one request to the scratch daemon; prints the reply
    "${DEMO_ENV[@]}" bash -c '
        source "$HOME/.sot-comm/bin/comm-lib.sh"
        ENDPOINT="unix:$SOT_SOCKET"
        frame="$(jq -nc --arg op "$1" --argjson p "$2" "{v:1,id:1,kind:\"req\",op:\$op,payload:\$p}")"
        sot_oneshot_request "$frame" "$1"' _ "$1" "$2"
}
row_type() {   # row_type <label> <command line> — typed into the row's shell, then Enter
    local b64; b64="$(printf '%s' "$2" | base64 -w0)"
    sotreq pty.input "$(jq -nc --arg w "${ROW_ID[$1]}" --arg d "$b64" '{workspace_id:$w,data_b64:$d,enter:true}')" \
        >>"$WORK/rows.log" 2>&1 || echo "row_type $1 failed (see $WORK/rows.log)" >&2
}
start_rows() {
    local label root reply id
    for label in "${SESSION_ROWS[@]}"; do
        root="$DHOME/$label"
        reply="$(sotreq workspace.create "$(jq -nc --arg l "$label" --arg r "$root" \
            '{label:$l,project_root:$r,agent:"none",boot:true}')")" \
            || die "workspace.create $label failed: $reply"
        id="$(jq -r '.payload.workspace_id // empty' <<<"$reply")"
        [ -n "$id" ] || die "workspace.create $label: no id in $reply"
        ROW_ID[$label]="$id"
        echo "create $label: $reply" >>"$WORK/rows.log"
    done
    # fe.command "workspace" names a row by its slug, not its id.
    reply="$(sotreq workspace.list '{}')" || die "workspace.list failed: $reply"
    for label in "${SESSION_ROWS[@]}"; do
        ROW_SLUG[$label]="$(jq -r --arg w "${ROW_ID[$label]}" \
            '[.. | objects | select(.workspace_id? == $w) | .slug // empty][0] // empty' <<<"$reply")"
        [ -n "${ROW_SLUG[$label]}" ] || die "no slug for $label in workspace.list: $reply"
    done
    sleep 3   # the capsules' shells come up
    # The strip ranks rows by work state, then by status stamp, newest first
    # (activity_order in the frontend). Joining a second apart gives every
    # row a distinct stamp, so the idle order is the same in every frame.
    for label in "${SESSION_ROWS[@]}"; do
        row_type "$label" "clear; comm-join.sh --name ${label,,} >/dev/null && comm-status.sh stop"
        ROW_STATE[$label]=idle
        sleep 1.1
    done
    sleep 1
}
# rows_state label:state ... — working = a turn in flight (the prompt hook),
# waiting / blocked / done = the declaration an agent makes, then the turn's
# stop; idle = stop alone (a declared blocked or done outlives it). Each
# row has its own why per state, so no two rows ever show the same text.
# A row already in the asked state is left alone: a restamp
# would move it in the strip.
declare -A ROW_STATE=()
declare -A WHY=(
  [analysis:working]="fitting leg bearings"     [analysis:waiting]="fit queued behind survey"
  [analysis:blocked]="which datum, WGS84?"      [analysis:done]="bearing table written"
  [figures:working]="drawing the track map"     [figures:waiting]="render job queued"
  [figures:blocked]="log scale on the y axis?"  [figures:done]="figures saved"
  [gpu-train:working]="epoch 12 of 40"          [gpu-train:waiting]="GPU busy, 2 jobs ahead"
  [gpu-train:blocked]="out of memory: halve the batch?" [gpu-train:done]="checkpoint saved"
  [survey:working]="merging 2023 transects"     [survey:waiting]="download 61%"
  [survey:blocked]="which survey year?"         [survey:done]="transects merged"
)
rows_state() {
    local entry label st why
    for entry in "$@"; do
        label="${entry%%:*}"; st="${entry#*:}"
        for l in "${SESSION_ROWS[@]}"; do [ "${l,,}" = "${label,,}" ] && label="$l"; done
        [ "${ROW_STATE[$label]:-}" = "$st" ] && continue
        ROW_STATE[$label]=$st
        why="${WHY[${label,,}:$st]:-}"
        [ "$st" = idle ] || [ -n "$why" ] || die "rows_state: no why for $label:$st"
        case "$st" in
            working) row_type "$label" "comm-status.sh prompt && comm-status.sh working \"$why\"" ;;
            waiting|blocked|done) row_type "$label" "comm-status.sh $st \"$why\" && comm-status.sh stop" ;;
            idle)    row_type "$label" 'comm-status.sh stop' ;;
            *) die "rows_state: unknown state $st" ;;
        esac
    done
}

# ---- daemon + frontend ---------------------------------------------------------
# One daemon for the whole run: the rows are live processes, not files.
DAEMON_PID=""; FE_PID=""
start_daemon() {
    local sock="$WORK/runtime/sotd.sock"; rm -f "${sock:?}"
    # cwd = project root: the REPL child inherits it (relative includes).
    HOSTWRAP_RW="${DAEMON_RW:-}" HOSTWRAP_CWD="$DEMO" spawn "$WORK/sotd.log" "${DEMO_ENV[@]}" \
        "$BIN/sotd" --socket "$sock" --project-root "$DHOME"
    DAEMON_PID=$SPAWNED
    for _ in $(seq 1 100); do [ -S "$sock" ] && break; sleep 0.1; done
    [ -S "$sock" ] || die "scratch sotd did not open its socket (see $WORK/sotd.log)"
}
# The frontend's persisted state, rewritten before every start: window
# geometry = the Xvfb screen (see header), resumed into the live DemoProject
# row. Not --ephemeral, which skips the workspace resume; --no-lease, so a
# take's end (stop_fe kills the window) never ends the scratch sessions
# between takes. What the frontend persists lands in the scratch XDG_CONFIG_HOME.
# FE_H (the hero take only) makes the window shorter than the screen; the
# recording still grabs the whole screen and the cut crops it.
start_fe() {   # start_fe flags...
    printf 'window_w = %s\nwindow_h = %s\nwindow_x = 0\nwindow_y = 0\nlast_host = "local"\nlast_workspace_id = "%s"\n' \
        "$W" "${FE_H:-$H}" "${ROW_ID[DemoProject]}" >"$WORK/config/sot/state-demo.toml"
    # shellcheck disable=SC2068 — flags are a curated word list
    spawn "$WORK/sot.log" "${DEMO_ENV[@]}" "$BIN/sot" --socket "$WORK/runtime/sotd.sock" \
        --font-scale "$FONT_SCALE" --no-lease $@
    FE_PID=$SPAWNED
}
# A resumed workspace does not attach its pane until a switch, and a switch
# to the current row is a no-op: switch away and back over the daemon's
# fe.command channel (what `sot-fe workspace` sends).
attach_row() {
    local slug
    for slug in "${ROW_SLUG[analysis]}" "${ROW_SLUG[DemoProject]}"; do
        sotreq fe.command.send "{\"cmd\":\"goto_workspace\",\"args\":{\"workspace\":\"$slug\"}}" >>"$WORK/rows.log" 2>&1 \
            || echo "attach_row failed (see $WORK/rows.log)" >&2
        sleep 1.5
    done
    # The attach leaves a pane-attach notice in the status line; a round
    # trip through Sessions mode restores the plain connection status.
    keys "sleep 1;key s;sleep 1;key f;sleep 1"
}
stop_fe() { [ -n "$FE_PID" ] && reap "$FE_PID"; FE_PID=""; }
grab() {       # grab <out.png>
    ffmpeg -loglevel error -y -f x11grab -draw_mouse 0 -video_size "${W}x${H}" \
        -i "$DISPLAY_NUM" -frames:v 1 "$1"
}
REC_PID=""
rec_start() {  # rec_start <raw.mkv> — lossless, stopped by rec_stop
    spawn "$WORK/ffmpeg-$(basename "$1").log" ffmpeg -loglevel error -y -f x11grab -draw_mouse 0 \
        -framerate "$FPS" -video_size "${W}x${H}" -i "$DISPLAY_NUM" \
        -c:v libx264rgb -crf 0 -preset ultrafast "$1"
    REC_PID=$SPAWNED; sleep 0.5
}
rec_stop() {   # SIGINT lets ffmpeg finish the container
    kill -INT "$REC_PID" 2>/dev/null || true
    for _ in $(seq 1 100); do kill -0 "$REC_PID" 2>/dev/null || break; sleep 0.1; done
    reap "$REC_PID"; REC_PID=""
}
