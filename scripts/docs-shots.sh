#!/usr/bin/env bash
# docs-shots.sh — generate the Tier-1 documentation screenshots.
#
#   scripts/docs-shots.sh sync-fixture   # re-stamp the fixture's fresh .concept hash
#   scripts/docs-shots.sh list           # print the shot matrix (for manual/Windows runs)
#   scripts/docs-shots.sh run [shot...]  # execute the matrix here (needs a display + GPU)
#
# Display-agnostic by design: `run` assumes the box it runs on can open a
# wgpu window (Linux desktop, Windows, macOS). It does NOT try xvfb — the FE
# is wgpu/vulkan, so a virtual X server alone can't render it. Boxes without
# a display use `list` to replicate the exact invocations elsewhere.
#
# Tier-1 = scripted, deterministic captures via the FE's --capture harness.
# Tier-2 (hero, orchestrator, terminal drawer) are hand-staged live grabs;
# recipes in docs/SCREENSHOTS.md.
#
# The sotd invocation is deliberately minimal (--socket + --project-root):
# resource-path resolution (kernel/repl/mathjax) is the daemon's own job.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$REPO/docs/fixtures/DemoProject"
OUT="$REPO/docs/src/assets/screenshots"
RELEASE="$REPO/rust/target/release"
SOT="$RELEASE/sot"; SOTD="$RELEASE/sotd"
RUNDIR="${TMPDIR:-/tmp}/sot-docs-shots.$$"

say() { printf '\033[1;36m==\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ---- shot matrix -------------------------------------------------------------
# name|project_root|extra FE flags
#   project_root: fixture = DemoProject, repo = this checkout (examples/preview),
#                 none = no backend (offline chrome).
# Add a row here to add a shot; `list` and `run` both read this table.
# Every shot also gets COMMON_FLAGS: docs shots are borderless-fullscreen —
# Ship of Tools is designed for ultrawide displays, so capture on one — at
# the maintainer's canonical font scale (1.1, 2026-07-03), pinned explicitly so the
# shots are identical on any capture box regardless of its zoom, settings,
# or monitor tier.
COMMON_FLAGS="--start-fullscreen --font-scale 1.1"
# Delays are >=12s on backend shots: the kernel is lazy-spawned and MathJax
# renders async (win-fe capture run, 2026-07-02). The daemon warm-up capture
# below absorbs the cold-start; delays still stay generous.
# concept-stale is a FILES-mode shot on src/DemoProject.jl: file-level targets
# are the ones the FE actually reads (files/<path>), file.parse fires only for
# cursored .jl files (that fills the hash the drift badge compares against),
# and function-level targets would need a colon in the filename — illegal on
# NTFS, so they can never exist in a Windows checkout.
MATRIX=(
  "state-colors|none|--demo-sessions DemoProject:working,black-mesa:idle,white-rabbit:waiting,zion:blocked,figures:done --demo-flash white-rabbit --contrast-mode bright --capture-delay-ms 1500"
  "nav-files|fixture|--start-mode files --start-selected 2 --auto-expand --capture-delay-ms 12000"
  "modules-methods|fixture|--start-mode modules --start-selected 6 --auto-expand --demo-function-methods DemoProject:route_length --capture-delay-ms 12000"
  "concept-stale|fixture|--start-mode files --start-path src/DemoProject.jl --capture-delay-ms 60000"
  "preview-math|fixture|--capture-preview README.md --capture-delay-ms 15000"
  "pin-sigil|fixture|--start-mode files --start-selected 2 --auto-pin --capture-delay-ms 12000"
  "preview-pdf|repo|--capture-preview examples/preview/sample.pdf --capture-delay-ms 60000"
  "preview-hdf5|repo|--capture-preview examples/preview/sample.h5 --capture-delay-ms 90000"
  "monitor-drawer|fixture|--start-monitor --capture-delay-ms 12000"
  "help-overlay|none|--start-help --capture-delay-ms 1500"
  # REPL figure: --demo-repl-eval submits through the FE's own path (external
  # protocol evals are dropped by design). The REPL boots in the SHIM env with
  # cwd = the daemon's cwd (pinned to the project root above), so the staged
  # script is a plain relative include — space-free for the word-split flags —
  # and stage_repl.jl itself activates the fixture env before `using`.
  # NB --capture-delay-ms is a FRAME count at nominal 60fps and capture
  # renders unthrottled (~166fps): 300000 nominal ~= 105s wall — REPL spawn +
  # package load + first plot (precompiles absorbed by the prep step).
  "repl-figure|fixture|--demo-repl-eval include(\"demo/stage_repl.jl\") --capture-delay-ms 300000"
)

# ---- sync-fixture ------------------------------------------------------------
# The FE's drift badge compares an annotation's `synced_against` against the
# kernel's file-level ast_hash, which today is sha256 of the raw file bytes
# (ShipToolsKernel.handle_file_parse). Stamp the FRESH annotation with the
# current hash; the STALE one (route_length.md, all zeros) is never touched.
sync_fixture() {
    local src="$FIXTURE/src/DemoProject.jl"
    local ann="$FIXTURE/.concept/modules/DemoProject.md"
    local hash
    hash="$(sha256sum "$src" | cut -d' ' -f1)"
    sed -i "s/^synced_against: .*/synced_against: \"$hash\"/" "$ann"
    say "stamped $ann"
    say "synced_against: $hash"
}

# ---- list --------------------------------------------------------------------
list_matrix() {
    echo "Tier-1 shot matrix (fullscreen on the capture monitor — use the ultrawide; output <name>.png):"
    echo
    for row in "${MATRIX[@]}"; do
        IFS='|' read -r name root flags <<<"$row"
        flags="$COMMON_FLAGS $flags"
        case "$root" in
            fixture) echo "[$name]  backend: sotd --socket <sock> --project-root docs/fixtures/DemoProject" ;;
            repo)    echo "[$name]  backend: sotd --socket <sock> --project-root <repo checkout>" ;;
            none)    echo "[$name]  backend: none (offline chrome)" ;;
        esac
        if [ "$root" = none ]; then
            echo "         sot --ephemeral --capture $name.png $flags"
        else
            echo "         sot --socket <sock> --capture $name.png $flags"
        fi
        echo
    done
    echo "Run 'scripts/docs-shots.sh sync-fixture' first so the fresh annotation hash matches."
    echo "Tier-2 (hand-staged) recipes: docs/SCREENSHOTS.md"
}

# ---- run ---------------------------------------------------------------------
DAEMON_PID=""
stop_daemon() {
    if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    DAEMON_PID=""
}
trap 'stop_daemon; rm -rf "$RUNDIR"' EXIT

# Transport: Windows sotd --socket wants a \\.\pipe\ named pipe path, which
# MSYS mangles ("\\"→"\", "not a named pipe path"); a loopback TCP port is the
# portable substrate. Unix/macOS keep the original unix socket.
CONN_FLAG="--socket"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) CONN_FLAG="--tcp" ;; esac
TCP_PORT=18780   # bumped per daemon start; avoids the live daemon's 18743
CONN_VALUE=""    # set by start_daemon (NOT echoed): a $(...) capture would run
                 # start_daemon in a SUBSHELL and lose both the DAEMON_PID it
                 # records (so stop_daemon becomes a no-op — daemons orphan) and
                 # the TCP_PORT it increments (so every daemon reuses one port —
                 # a later root-switch then connects to the wrong stale daemon).

start_daemon() { # $1 = project root; sets globals DAEMON_PID + CONN_VALUE. Call it
                 # directly (start_daemon "$x"; v="$CONN_VALUE"), NEVER via $(...).
    # cwd = project root, deliberately: daemon children (the REPL shim)
    # inherit it, which is what lets --demo-repl-eval use the relative
    # include("demo/stage_repl.jl"). Manual/PowerShell replication must
    # also start the scratch sotd FROM the project root.
    if [ "$CONN_FLAG" = "--tcp" ]; then
        TCP_PORT=$((TCP_PORT + 1))
        local ep="127.0.0.1:$TCP_PORT"
        (cd "$1" && exec "$SOTD" --tcp "$ep" --project-root "$1") >"$RUNDIR/sotd.log" 2>&1 &
        DAEMON_PID=$!
        local ok=""
        for _ in $(seq 1 50); do
            (exec 3<>"/dev/tcp/127.0.0.1/$TCP_PORT") 2>/dev/null && { exec 3>&-; ok=1; break; }
            kill -0 "$DAEMON_PID" 2>/dev/null || break
            sleep 0.2
        done
        [ -n "$ok" ] || die "scratch sotd did not open tcp $ep (see $RUNDIR/sotd.log)"
        CONN_VALUE="$ep"
    else
        local sock="$RUNDIR/sotd.sock"
        rm -f "$sock"
        (cd "$1" && exec "$SOTD" --socket "$sock" --project-root "$1") >"$RUNDIR/sotd.log" 2>&1 &
        DAEMON_PID=$!
        for _ in $(seq 1 50); do [ -S "$sock" ] && break; sleep 0.2; done
        [ -S "$sock" ] || die "scratch sotd did not open $sock (see $RUNDIR/sotd.log)"
        CONN_VALUE="$sock"
    fi
}

run_matrix() {
    mkdir -p "$OUT" "$RUNDIR"

    # Isolate capture FEs from the user's real session state, or a repo-root
    # daemon resumes the last SUB-workspace (e.g. DemoProject) instead of its
    # default (the repo) and repo-relative --capture-preview paths fall back to
    # the root placeholder. BOTH dirs matter: XDG_CONFIG_HOME holds
    # state-persistence (last workspace/nav/font — state_persistence::config_dir),
    # XDG_STATE_HOME holds the transport session memory (client_id/session_id —
    # state::session_path); leaving the latter shared makes the FE present the
    # live client's session and resume its workspace despite a clean config.
    export XDG_CONFIG_HOME="$RUNDIR/config"
    export XDG_STATE_HOME="$RUNDIR/state"
    mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"

    # Binaries: build only if absent (a capture box is usually a dev box).
    if [ ! -x "$SOT" ] || [ ! -x "$SOTD" ]; then
        say "building rust workspace (release)…"
        cargo build --release --manifest-path "$REPO/rust/Cargo.toml"
    fi
    # Three Julia envs or the shots silently degrade: the kernel (daemon
    # spawn fails without it), the REPL SHIM (its Manifest is gitignored —
    # a fresh box's repl child dies at `using ShipToolsRepl` and every eval
    # sits at "(running…)" forever), and the fixture (repl-figure's
    # CairoMakie; pre-instantiating here keeps the Makie precompile out of
    # the capture window).
    say "instantiating kernel + repl-shim + fixture envs…"
    julia --project="$REPO/julia/kernel" -e 'using Pkg; Pkg.instantiate()' >/dev/null
    julia --project="$REPO/julia/repl"   -e 'using Pkg; Pkg.instantiate()' >/dev/null
    julia --project="$FIXTURE"           -e 'using Pkg; Pkg.instantiate()' >/dev/null
    # MathJax sidecar: without node_modules, math previews show raw LaTeX
    # (win-fe capture run found this the hard way).
    if [ ! -d "$REPO/rust/backend/sidecars/mathjax/node_modules" ]; then
        say "installing mathjax sidecar deps (npm ci)…"
        (cd "$REPO/rust/backend/sidecars/mathjax" && npm ci --silent)
    fi
    # PDF previews shell out to poppler; without it preview-pdf silently
    # degrades to the fallback rendering.
    command -v pdftoppm >/dev/null && command -v pdfinfo >/dev/null \
        || die "poppler (pdftoppm + pdfinfo) not on PATH — required for preview-pdf (Windows: conda install poppler)"

    sync_fixture

    local only=("$@") current_root="" sock=""
    for row in "${MATRIX[@]}"; do
        IFS='|' read -r name root flags <<<"$row"
        flags="$COMMON_FLAGS $flags"
        if [ ${#only[@]} -gt 0 ]; then
            case " ${only[*]} " in *" $name "*) ;; *) continue ;; esac
        fi
        say "shot: $name"
        if [ "$root" = none ]; then
            stop_daemon; current_root=""
            # shellcheck disable=SC2086 — flags are a curated word list
            "$SOT" --ephemeral --capture "$OUT/$name.png" $flags
        else
            local want="$FIXTURE"; [ "$root" = repo ] && want="$REPO"
            if [ "$current_root" != "$want" ]; then
                stop_daemon
                start_daemon "$want"; sock="$CONN_VALUE"
                current_root="$want"
                # Warm-up capture (discarded): absorbs the lazy kernel spawn +
                # MathJax sidecar boot, and for the repo root the known
                # first-HDF5-render crash/respawn cycle — so every kept shot
                # renders against a warm stack. The fixture warm-up must
                # actually TOUCH the kernel: --capture-preview alone is
                # daemon-side file IO and leaves the kernel cold (which is
                # how the first concept-stale runs timed out on Windows), so
                # walk to the .jl via --start-path — that fires file.parse
                # and forces the kernel spawn.
                say "warming daemon ($root root)…"
                if [ "$root" = repo ]; then
                    # Long warm-up: on a fast GPU (~166fps) the frame-count delay
                    # burns wall-time quickly, so the repo root needs a generous
                    # window to (a) settle the FE onto the daemon's default
                    # workspace (ship-of-tools, not the scanned DemoProject
                    # sub-workspace) and (b) absorb the kernel spawn + the lazy
                    # first in-process HDF5Preview load before the kept H5 shot.
                    "$SOT" $CONN_FLAG "$sock" --capture "$RUNDIR/warmup.png" \
                        --start-fullscreen --capture-preview examples/preview/sample.h5 \
                        --capture-delay-ms 150000 || true
                else
                    "$SOT" $CONN_FLAG "$sock" --capture "$RUNDIR/warmup.png" \
                        --start-fullscreen --start-mode files \
                        --start-path src/DemoProject.jl \
                        --capture-delay-ms 30000 || true
                fi
            fi
            # shellcheck disable=SC2086
            "$SOT" $CONN_FLAG "$sock" --capture "$OUT/$name.png" $flags
        fi
        [ -s "$OUT/$name.png" ] || die "capture produced no PNG for $name"
    done
    stop_daemon

    if command -v oxipng >/dev/null; then
        say "optimizing PNGs (oxipng)…"
        oxipng -q -o 4 --strip safe "$OUT"/*.png
    elif command -v pngquant >/dev/null; then
        say "optimizing PNGs (pngquant)…"
        pngquant --force --skip-if-larger --ext .png 128 "$OUT"/*.png || true
    else
        say "no oxipng/pngquant found — PNGs left unoptimized"
    fi
    say "done → $OUT"
}

case "${1:-}" in
    sync-fixture) sync_fixture ;;
    list)         list_matrix ;;
    run)          shift; run_matrix "$@" ;;
    *)            die "usage: docs-shots.sh sync-fixture | list | run [shot...]" ;;
esac
