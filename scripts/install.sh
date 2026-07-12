#!/usr/bin/env bash
# install.sh — Ship of Tools installer for Linux/macOS (ADR 0030 §5).
#
#   curl -fsSL <raw-url>/scripts/install.sh | bash -s -- --local
#   ./scripts/install.sh --local                     # all-in-one on this box
#   ./scripts/install.sh --backend <ssh-alias>       # FE here → remote BE
#   ./scripts/install.sh --be-only                   # headless backend/canary
#   [--version vX.Y.Z] [--prefix <dir>]              # default: latest release when assets exist
#
# What it does (idempotent; re-run to upgrade):
#   1. preflight — arch/glibc floor for the FE, tar/curl present (gh or
#      $GITHUB_TOKEN are OPTIONAL: authed calls dodge API rate limits)
#   2. download the release artifacts + verify SHA256SUMS
#   3. lay out $PREFIX (~/.local/share/sot): bin/ updates/ repo/current
#   4. REPO CHECKOUT at the release tag (ADR 0030 addendum: the repo IS the
#      manual and the resource tree; blobless partial clone = full history
#      for blame, only the tag's tree downloaded; supersedes the curated
#      julia bundle) + juliaup + Pkg.instantiate inside the checkout
#   5. config in ~/.config/sot (hosts.toml, settings.toml) — never clobbers
#      an existing file
#   6. agent comm resources: ~/.sot-comm plus Claude/Codex skills
#   7. backend roles: install+enable the systemd --user sotd unit
#   8. FE roles: ~/.local/bin/sot-launch wrapper + app/desktop entry
#
# Development machines DON'T use this release installer — they run from a checkout.
set -euo pipefail

REPO="${SOT_INSTALL_REPO:-kalidke/ship-of-tools}"
PREFIX="${SOT_PREFIX:-$HOME/.local/share/sot}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/sot"
ROLE="" VERSION="" BE_ALIAS="" PORT=18743 NO_SERVICE=0
GLIBC_FLOOR_FE="2.35"

say()  { printf '\033[1;36m==\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --local) ROLE=local ;;
        --backend) ROLE=remote; BE_ALIAS="${2:?--backend needs an ssh alias}"; shift ;;
        --be-only) ROLE=be-only ;;
        --version) VERSION="${2:?}"; shift ;;
        --prefix) PREFIX="${2:?}"; shift ;;
        --port) PORT="${2:?}"; shift ;;
        # Skip the systemd unit install/enable — for shared-home deployments
        # (a user-level unit file + its enable symlink live in $HOME, so on an
        # shared home they'd apply to EVERY machine). The caller supervises
        # sotd itself (e.g. systemd-run --user transient unit, per-machine).
        --no-service) NO_SERVICE=1 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
    shift
done
# No role flag → interactive Q&A (matches the sot-setup experience; found
# missing by the first real laptop install). Prompts read /dev/tty so this
# also works under `curl | bash`. No TTY and no flags → the old hard error.
if [ -z "$ROLE" ]; then
    # A real open-probe: -r/-w pass on the device node even in ttyless
    # contexts (cron, CI, piped bash) where opening it then fails.
    if (: < /dev/tty) 2>/dev/null && (: > /dev/tty) 2>/dev/null; then
        {
            echo "Where should Ship of Tools run?"
            echo "  1) all on this machine        (frontend + backend here)"
            echo "  2) frontend here, backend on another machine over SSH"
            echo "  3) backend only on this machine (headless server)"
            printf "Choose [1-3]: "
        } > /dev/tty
        read -r choice < /dev/tty
        case "$choice" in
            1) ROLE=local ;;
            2) ROLE=remote
               printf "SSH alias/hostname of the backend machine (key-based auth required): " > /dev/tty
               read -r BE_ALIAS < /dev/tty
               [ -n "$BE_ALIAS" ] || die "backend host is required for the remote layout"
               printf "Verifying ssh to '%s'... " "$BE_ALIAS" > /dev/tty
               ssh -o BatchMode=yes -o ConnectTimeout=8 "$BE_ALIAS" true 2>/dev/null \
                   && echo "ok" > /dev/tty \
                   || { echo "FAILED" > /dev/tty; die "key-based ssh to '$BE_ALIAS' doesn't work (ssh-copy-id first, or fix ~/.ssh/config)"; } ;;
            3) ROLE=be-only ;;
            *) die "no such choice: '$choice'" ;;
        esac
        printf "Local tunnel port [%s]: " "$PORT" > /dev/tty
        read -r p < /dev/tty
        [ -n "$p" ] && PORT="$p"
    else
        echo "pick a role: --local | --backend <alias> | --be-only (no TTY for interactive setup)" >&2
        exit 2
    fi
fi

# ---- 1. preflight ------------------------------------------------------------
OS="$(uname -s)"
case "$OS" in
    Linux)
        [ "$(uname -m)" = x86_64 ] || die "the linux prebuilt is x86_64 only (this is $(uname -m)) — build from source"
        TARGET="linux-x86_64" ;;
    Darwin)
        [ "$(uname -m)" = arm64 ] || die "the macOS prebuilt is Apple Silicon only (this is $(uname -m)) — Intel Macs build from source"
        TARGET="macos-aarch64"
        say "macOS support is EXPERIMENTAL — both roles work; please report anything broken" ;;
    MINGW*|MSYS*|CYGWIN*)
        die "this installer covers Linux and macOS. Windows uses source setup unless a release zip exists; see docs/INSTALL-AGENT.md section 2b" ;;
    *)  die "unsupported OS $OS. Linux/macOS: this installer; Windows: docs/INSTALL-AGENT.md section 2b" ;;
esac
for t in curl tar; do command -v "$t" >/dev/null || die "$t is required"; done

if [ "$OS" = Linux ] && [ "$ROLE" != be-only ]; then
    # No pipelines here: `... | head | grep || echo 0` SIGPIPEs ldd under
    # pipefail and APPENDS a bogus "0" to a good match, making the floor
    # check fail on EVERY machine (the first real laptop install hit this).
    # Capture the whole output, then parse from the variable.
    ldd_out="$(ldd --version 2>/dev/null || true)"
    glibc="$(printf '%s\n' "$ldd_out" | sed -n '1s/.*[^0-9.]\([0-9][0-9]*\.[0-9][0-9]*\)[[:space:]]*$/\1/p')"
    [ -n "$glibc" ] || glibc=0
    lowest="$(printf '%s\n%s\n' "$GLIBC_FLOOR_FE" "$glibc" | sort -V | sed -n 1p)"
    [ "$lowest" = "$GLIBC_FLOOR_FE" ] \
        || die "the frontend binary needs glibc >= $GLIBC_FLOOR_FE (this box: $glibc). The backend (musl, --be-only) runs anywhere."
fi

# tmux is a HARD runtime dependency of the backend — the daemon hosts the LLM
# pane in a tmux session. Only checked for roles that run sotd on THIS machine
# (local, be-only); --backend points at a remote daemon, so a local tmux is not
# needed. A missing tmux is fatal here (nothing surfaced it before — the first
# real server install found tmux entirely unmentioned). tmux < 3.2 is a graceful
# DEGRADE, not an error: the daemon version-gates `new-session -e` (older tmux
# rejected it at arg-parse and drove a respawn storm — expectations 2026-07-11),
# so the backend runs, but the pane's in-session SOT_* awareness is best-effort.
if [ "$ROLE" = local ] || [ "$ROLE" = be-only ]; then
    command -v tmux >/dev/null 2>&1 \
        || die "tmux is required for the backend (the daemon hosts the LLM pane in a tmux session) but is not on PATH. Install it (e.g. 'sudo apt install tmux', or a user-local tmux >= 3.2 in ~/.local/bin) and re-run."
    # Parse "tmux 3.0a" / "tmux next-3.4" -> "3.0" / "3.4". No pipefail traps
    # here (single sed, no head/grep pipeline).
    tmux_ver="$(tmux -V 2>/dev/null | sed -n '1s/^tmux \(next-\)\{0,1\}\([0-9][0-9]*\.[0-9][0-9]*\).*/\2/p')"
    [ -n "$tmux_ver" ] || tmux_ver=0
    tmux_lowest="$(printf '%s\n%s\n' "3.2" "$tmux_ver" | sort -V | sed -n 1p)"
    if [ "$tmux_lowest" != "3.2" ]; then
        say "NOTE: tmux $tmux_ver (< 3.2) detected. The backend runs fine, but the LLM pane's in-session Ship of Tools awareness env is best-effort only on old tmux. For full awareness put a tmux >= 3.2 earlier on the daemon's PATH (e.g. ~/.local/bin). See docs/INSTALL-AGENT.md."
    fi
fi

# Downloader: for a public repo, unauthenticated curl works. gh (authed) is
# preferred when present, and $GITHUB_TOKEN is honored purely to dodge the
# unauthenticated API rate limit (60 req/h per IP).
FETCH=curl
if command -v gh >/dev/null && gh auth status >/dev/null 2>&1; then
    FETCH=gh
fi

# The curl path parses GitHub's JSON — needs jq (the gh path doesn't).
[ "$FETCH" = curl ] && { command -v jq >/dev/null || die "jq is required (or install+auth gh)"; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-install.XXXXXX")"; trap 'rm -rf "$WORK"' EXIT
gh_api() {  # gh_api <endpoint> <outfile> — API GET to a file (token optional)
    curl -fsSL ${GITHUB_TOKEN:+-H "Authorization: Bearer $GITHUB_TOKEN"} \
         -H "Accept: application/vnd.github+json" \
         -o "$2" "https://api.github.com/repos/$REPO/$1"
}

if [ -z "$VERSION" ]; then
    if [ "$FETCH" = gh ]; then
        VERSION="$(gh api "repos/$REPO/releases/latest" --jq .tag_name)"
    else
        gh_api "releases/latest" "$WORK/latest.json"
        VERSION="$(jq -r .tag_name "$WORK/latest.json")"
    fi
fi
VER="${VERSION#v}"
say "installing Ship of Tools $VERSION (role: $ROLE) into $PREFIX"

# ---- 2. download + verify ----------------------------------------------------
ASSETS=("SHA256SUMS" "sot-$VER-$TARGET.tar.gz")

dl() {
    if [ "$FETCH" = gh ]; then
        gh release download "$VERSION" -R "$REPO" -p "$1" -D "$WORK"
    else
        [ -f "$WORK/release.json" ] || gh_api "releases/tags/$VERSION" "$WORK/release.json"
        url="$(jq -r --arg n "$1" '.assets[] | select(.name == $n) | .url' "$WORK/release.json")"
        [ -n "$url" ] && [ "$url" != null ] || die "asset $1 not found on release $VERSION"
        curl -fsSL ${GITHUB_TOKEN:+-H "Authorization: Bearer $GITHUB_TOKEN"} -H "Accept: application/octet-stream" -o "$WORK/$1" "$url"
    fi
}
say "downloading ${#ASSETS[@]} assets"
for a in "${ASSETS[@]}"; do dl "$a"; done
if command -v sha256sum >/dev/null; then
    ( cd "$WORK" && sha256sum -c --ignore-missing SHA256SUMS ) || die "checksum verification FAILED"
else
    ( cd "$WORK" && shasum -a 256 -c --ignore-missing SHA256SUMS ) || die "checksum verification FAILED"
fi

# ---- 3. layout ---------------------------------------------------------------
mkdir -p "$PREFIX/bin" "$PREFIX/updates" "$PREFIX/repo" "$CONFIG" "$HOME/.local/bin"
tar -xzf "$WORK/sot-$VER-$TARGET.tar.gz" -C "$WORK"
BINDIR="$WORK/sot-$VER-$TARGET"
for b in sot sotd; do
    [ -f "$PREFIX/bin/$b" ] && cp "$PREFIX/bin/$b" "$PREFIX/bin/$b.prev"
    install -m 0755 "$BINDIR/$b" "$PREFIX/bin/$b"
    # Gatekeeper: strip any quarantine attr (browser downloads carry it).
    [ "$OS" = Darwin ] && xattr -d com.apple.quarantine "$PREFIX/bin/$b" 2>/dev/null || true
done
say "binaries: $("$PREFIX/bin/sotd" --version)"
DEFAULT_SOCKET="$("$PREFIX/bin/sotd" session-socket-path sot)"

# ---- 4. the repo checkout — manual, resources, julia code (ADR 0030 add.) -----
# The checkout at $PREFIX/repo/current IS the product's resource tree and its
# help system: resource_dir resolves julia/kernel, julia/repl, sidecars, and
# examples from it, and the FE Terminal's agent reads docs/ + ADRs + source as
# the manual. Blobless partial clone (--filter=blob:none): full history for
# blame, only the tag's tree downloaded. Update = re-run this installer with
# the new version — fetch tags + checkout moves the same tree. READ-ONLY BY
# CONVENTION: a dirty tree refuses to move (fail loud).
CHECKOUT="$PREFIX/repo/current"
command -v git >/dev/null || die "git is required (the install includes a repo checkout)"
if [ -d "$CHECKOUT/.git" ]; then
    if [ -n "$(git -C "$CHECKOUT" status --porcelain)" ]; then
        die "the checkout at $CHECKOUT has local changes — commit/stash/revert, then re-run (updates refuse to move a dirty tree)"
    fi
    say "updating checkout to $VERSION"
    # --force on the tag fetch: a release tag force-moved upstream (e.g. the
    # public-flip history rewrite) otherwise makes the whole fetch abort with
    # "would clobber existing tag", blocking every upgrade re-run (issue #4).
    # The checkout is READ-ONLY BY CONVENTION, so force-updating tags is safe.
    git -C "$CHECKOUT" fetch --tags --force --filter=blob:none origin || die "fetch failed"
    git -C "$CHECKOUT" checkout -q "$VERSION" || die "checkout $VERSION failed"
else
    say "cloning repo at $VERSION (blobless partial clone)"
    if [ "$FETCH" = gh ]; then
        git -c "credential.helper=!gh auth git-credential" \
            clone --filter=blob:none --branch "$VERSION" "https://github.com/$REPO" "$CHECKOUT" \
            || die "clone failed"
    else
        # Public repo: plain https clone, no auth needed.
        git clone --filter=blob:none --branch "$VERSION" \
            "https://github.com/$REPO" "$CHECKOUT" \
            || die "clone failed"
    fi
fi
# Enforce HEAD == the tag's recorded commit (fresh clone AND update): a moved
# tag, wrong ref, or half-checkout must fail HERE, not at first use.
want="$(git -C "$CHECKOUT" rev-parse "refs/tags/$VERSION^{commit}" 2>/dev/null)" \
    || die "tag $VERSION not present in the checkout"
have="$(git -C "$CHECKOUT" rev-parse HEAD)"
[ "$have" = "$want" ] || die "checkout HEAD ($have) != $VERSION commit ($want) — refusing"
# Compat: older pre-clone binaries resolve resources via julia/current
# (the retired bundle's mount point). Point it at the checkout — repo-shaped
# either way — so the clone-based install works with any binary generation.
mkdir -p "$PREFIX/julia"
ln -sfn "$CHECKOUT" "$PREFIX/julia/current"

# ---- 5. Julia + agent comm -----------------------------------------------------
chan="1.12"
if [ -x "$HOME/.juliaup/bin/juliaup" ]; then
    export PATH="$HOME/.juliaup/bin:$PATH"
fi
if command -v julia >/dev/null && julia -e 'exit(VERSION >= v"1.12" ? 0 : 1)' >/dev/null 2>&1; then
    :
elif command -v juliaup >/dev/null 2>&1; then
    say "installing Julia channel $chan with juliaup"
    juliaup add "$chan" >/dev/null 2>&1 || true
    export PATH="$HOME/.juliaup/bin:$PATH"
else
    say "installing Julia (juliaup, channel $chan)"
    curl -fsSL https://install.julialang.org | sh -s -- --yes --default-channel "$chan"
    export PATH="$HOME/.juliaup/bin:$PATH"
fi
command -v julia >/dev/null || die "Julia install failed; julia is still not on PATH"
julia_run() {
    julia "+$chan" "$@" 2>/dev/null || julia "$@"
}

say "installing agent comm resources (sot-comm, Claude/Codex skills)"
julia_run --project="$CHECKOUT" -e 'using ShipTools; ShipTools.update_comm()' \
    || die "ShipTools.update_comm() failed"

if [ "$ROLE" != remote ]; then
    say "instantiating julia envs (first run takes a few minutes)"
    julia_run --project="$CHECKOUT/julia/kernel" -e 'using Pkg; Pkg.instantiate()'
    julia_run --project="$CHECKOUT/julia/repl" -e 'using Pkg; Pkg.instantiate()'
    julia_run --project="$CHECKOUT/julia/pluto" -e 'using Pkg; Pkg.instantiate(); Pkg.precompile(); using Pluto'

    # MathJax sidecar (math rendering in markdown previews). Its node deps
    # are NOT in the repo — without them every math.render dies with
    # "mathjax sidecar terminated" (bit a live deployment 2026-07-10).
    # Best-effort: a box without node still installs fine, math previews
    # just show raw LaTeX until the deps land.
    if command -v npm >/dev/null 2>&1; then
        say "installing MathJax sidecar deps (npm ci)"
        (cd "$CHECKOUT/rust/backend/sidecars/mathjax" && npm ci --silent) \
            || say "WARN: npm ci failed in sidecars/mathjax — math rendering unavailable until you run it manually"
    else
        say "WARN: node/npm not found — math rendering in markdown previews needs it."
        say "      Install node, then run: (cd $CHECKOUT/rust/backend/sidecars/mathjax && npm ci)"
    fi
fi

# ---- 6. config -----------------------------------------------------------------
# Non-clobbering on a same-role re-run, but a ROLE CHANGE reconfigures: the
# existing hosts.toml is backed up and rewritten (the first laptop install
# picked the wrong topology and a re-run couldn't heal it — never again).
if [ -f "$CONFIG/hosts.toml" ]; then
    want="local"; [ "$ROLE" = remote ] && want="$BE_ALIAS"
    if ! grep -q "^default_host = \"$want\"" "$CONFIG/hosts.toml"; then
        cp "$CONFIG/hosts.toml" "$CONFIG/hosts.toml.bak"
        rm "$CONFIG/hosts.toml"
        say "role changed — existing hosts.toml backed up to hosts.toml.bak and rewritten"
    fi
fi
# A remote-FE role must not leave a previously-installed LOCAL backend
# running (the wrong-topology remnant): disable it, don't just orphan it.
if [ "$ROLE" = remote ] && command -v systemctl >/dev/null 2>&1 && systemctl --user is-enabled sotd.service >/dev/null 2>&1; then
    systemctl --user disable --now sotd.service || true
    say "disabled the local sotd.service from a previous all-in-one install"
fi
if [ ! -f "$CONFIG/hosts.toml" ]; then
    case "$ROLE" in
        local|be-only) cat > "$CONFIG/hosts.toml" <<EOF
default_host = "local"

# Local backend on the per-user socket — no SSH involved for the same-machine role.
[host.local]
socket = "$DEFAULT_SOCKET"
EOF
        ;;
        remote) cat > "$CONFIG/hosts.toml" <<EOF
default_host = "$BE_ALIAS"

[host.$BE_ALIAS]
ssh_alias = "$BE_ALIAS"
remote_repo = "\$HOME"
tcp_port = $PORT
EOF
        ;;
    esac
    say "wrote $CONFIG/hosts.toml"
fi
[ -f "$CONFIG/settings.toml" ] || printf '# Ship of Tools settings — see settings.toml.example in the repo\n' > "$CONFIG/settings.toml"

# ---- 7. backend service --------------------------------------------------------
if [ "$ROLE" != remote ] && [ "$NO_SERVICE" = 1 ]; then
    say "skipping systemd unit (--no-service) — supervise sotd yourself, e.g.:"
    say "  systemd-run --user --unit=sotd-canary -p Restart=always $PREFIX/bin/sotd --project-root \$HOME --label sot"
fi
if [ "$OS" = Darwin ] && [ "$ROLE" != remote ]; then
    # No launchd wiring yet (roadmap): the local-role launcher below starts
    # sotd on demand; be-only Macs run it by hand.
    NO_SERVICE=1
    say "macOS: no service manager wiring yet — the sot-launch wrapper starts sotd on demand"
    [ "$ROLE" = be-only ] && say "  be-only: start it with  $PREFIX/bin/sotd --project-root ~ --label sot"
fi
if [ "$OS" = Linux ] && [ "$ROLE" != remote ] && [ "$NO_SERVICE" = 0 ]; then
    mkdir -p "$HOME/.config/systemd/user"
    sed -e "s|@SOT_BIN@|$PREFIX/bin/sotd|" \
        -e "s|@SOT_PROJECT_ROOT@|$HOME|" \
        "$BINDIR/sotd.service" > "$HOME/.config/systemd/user/sotd.service"
    systemctl --user daemon-reload
    systemctl --user enable --now sotd.service
    loginctl enable-linger "$USER" 2>/dev/null || true
    say "sotd running: $(systemctl --user is-active sotd.service) (socket $DEFAULT_SOCKET)"
fi

# ---- 8. FE launcher -------------------------------------------------------------
if [ "$ROLE" != be-only ]; then
    if [ "$ROLE" = local ]; then
        cat > "$HOME/.local/bin/sot-launch" <<EOF
#!/usr/bin/env bash
# All-in-one launcher: start the backend on demand if its per-user socket is
# missing (macOS has no service wiring yet; Linux normally has the systemd
# unit), then launch the frontend.
SOCKET="\$("$PREFIX/bin/sotd" session-socket-path sot)"
socket_open() {
    [ -S "\$SOCKET" ] || return 1
    if command -v nc >/dev/null 2>&1; then
        nc -U "\$SOCKET" </dev/null >/dev/null 2>&1 &
        pid=\$!
        sleep 1
        if kill -0 "\$pid" 2>/dev/null; then
            kill "\$pid" 2>/dev/null || true
            wait "\$pid" 2>/dev/null || true
            return 0
        fi
        wait "\$pid"
        return \$?
    fi
    # Minimal installs may not have nc. A socket file is the best available
    # probe; the frontend will still fail loud if the connect cannot complete.
    return 0
}
if ! socket_open; then
    rm -f "\$SOCKET" 2>/dev/null || true
    nohup "$PREFIX/bin/sotd" --project-root "\$HOME" --label sot >/tmp/sotd.log 2>&1 </dev/null &
    i=0; while [ \$i -lt 40 ]; do socket_open && break; sleep 0.25; i=\$((i+1)); done
    socket_open || { echo "ERROR: backend did not open \$SOCKET; see /tmp/sotd.log" >&2; exit 1; }
fi
exec "$PREFIX/bin/sot" --socket "\$SOCKET"
EOF
    else
        cat > "$HOME/.local/bin/sot-launch" <<EOF
#!/usr/bin/env bash
# FE → remote BE over SSH local forwards (key auth required, ADR 0030 §5).
# The frontend connects to local TCP $PORT; ssh terminates that forward at
# the remote user's per-user sotd socket. Aux services remain forwarded by
# local TCP ports for browser/webview compatibility.
REMOTE_SOCKET="\${SOT_REMOTE_SOCKET:-}"
if [ -z "\$REMOTE_SOCKET" ]; then
    REMOTE_SOCKET="\$(ssh "$BE_ALIAS" '\${SOT_REMOTE_SOTD:-\$HOME/.local/share/sot/bin/sotd} session-socket-path sot')" \
        || { echo "ERROR: could not query remote sotd socket path" >&2; exit 1; }
fi
port_open() {
    if (exec 3<>"/dev/tcp/127.0.0.1/\$1") 2>/dev/null; then exec 3>&-; return 0; fi
    command -v nc >/dev/null 2>&1 && nc -z 127.0.0.1 "\$1" >/dev/null 2>&1
}
ensure_aux_tunnel() {
    missing=()
    # 1234 pluto · 1235 video · 1236 docs · 1237-1240 docs pool · 1241 WGLMakie (ADR 0032)
    for p in 1234 1235 1236 1237 1238 1239 1240 1241; do
        port_open "\$p" || missing+=("\$p")
    done
    if [ "\${#missing[@]}" -eq 0 ]; then return 0; fi
    if [ "\${#missing[@]}" -ne 8 ]; then
        echo "ERROR: only some browser aux ports are open; missing: \${missing[*]}" >&2
        echo "       stop stale tunnels/services or free ports 1234-1241" >&2
        exit 1
    fi
    ssh -fN -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 \
      -L 1234:127.0.0.1:1234 -L 1235:127.0.0.1:1235 -L 1236:127.0.0.1:1236 \
      -L 1237:127.0.0.1:1237 -L 1238:127.0.0.1:1238 -L 1239:127.0.0.1:1239 -L 1240:127.0.0.1:1240 \
      -L 1241:127.0.0.1:1241 \
      "$BE_ALIAS" \
      || { echo "ERROR: could not open browser aux SSH tunnel" >&2; exit 1; }
}
if pgrep -f "ssh .*${PORT}:\$REMOTE_SOCKET.*$BE_ALIAS" >/dev/null 2>&1; then
    :
elif port_open "$PORT"; then
    echo "ERROR: local port $PORT is open but not forwarding to \$REMOTE_SOCKET" >&2
    exit 1
else
    ssh -fN -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 \
      -L "$PORT:\$REMOTE_SOCKET" \
      -L 1234:127.0.0.1:1234 -L 1235:127.0.0.1:1235 -L 1236:127.0.0.1:1236 \
      -L 1237:127.0.0.1:1237 -L 1238:127.0.0.1:1238 -L 1239:127.0.0.1:1239 -L 1240:127.0.0.1:1240 \
      -L 1241:127.0.0.1:1241 \
      "$BE_ALIAS" \
      || { echo "ERROR: could not open SSH tunnel" >&2; exit 1; }
fi
ensure_aux_tunnel
exec "$PREFIX/bin/sot" --tcp "127.0.0.1:$PORT"
EOF
    fi
    chmod +x "$HOME/.local/bin/sot-launch"
    if [ "$OS" = Darwin ]; then
        # A minimal .app bundle so the FE launches from Launchpad/Spotlight/
        # Dock like a real app. Locally-created bundles carry no quarantine
        # attr, so Gatekeeper doesn't object. The icon is generated from the
        # checkout's logo with sips+iconutil (both ship with macOS).
        APP="$HOME/Applications/Ship of Tools.app"
        mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
        cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleName</key><string>Ship of Tools</string>
    <key>CFBundleIdentifier</key><string>dev.ship-of-tools.sot</string>
    <key>CFBundleExecutable</key><string>sot-launch</string>
    <key>CFBundleIconFile</key><string>sot</string>
    <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
        cat > "$APP/Contents/MacOS/sot-launch" <<EOF
#!/usr/bin/env bash
exec "$HOME/.local/bin/sot-launch"
EOF
        chmod +x "$APP/Contents/MacOS/sot-launch"
        LOGO="$CHECKOUT/logo.png"
        if [ -f "$LOGO" ] && command -v sips >/dev/null && command -v iconutil >/dev/null; then
            ICONSET="$WORK/sot.iconset"; mkdir -p "$ICONSET"
            for sz in 16 32 128 256 512; do
                sips -z "$sz" "$sz" "$LOGO" --out "$ICONSET/icon_${sz}x${sz}.png" >/dev/null 2>&1 || true
                sips -z "$((sz*2))" "$((sz*2))" "$LOGO" --out "$ICONSET/icon_${sz}x${sz}@2x.png" >/dev/null 2>&1 || true
            done
            iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/sot.icns" 2>/dev/null                 && say "app icon generated from the checkout logo"                 || say "icon generation failed (cosmetic) — bundle works without it"
        fi
        say "FE launcher: sot-launch + 'Ship of Tools' in ~/Applications (Launchpad/Spotlight)"
    else
    mkdir -p "$HOME/.local/share/applications"
    cat > "$HOME/.local/share/applications/ship-of-tools.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Ship of Tools
Exec=$HOME/.local/bin/sot-launch
Terminal=false
Categories=Development;
EOF
    say "FE launcher: sot-launch (+ desktop entry)"
    fi
fi

say "DONE — Ship of Tools $VERSION installed ($ROLE)."
[ "$ROLE" = remote ] && say "reminder: key-based ssh to '$BE_ALIAS' is required (ssh $BE_ALIAS true)"
exit 0
