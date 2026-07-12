# Ship of Tools — agent install runbook

You are a coding agent (Claude Code, Codex, or similar) with shell access,
asked to install **Ship of Tools** — an agentic development system for Julia
(https://github.com/kalidke/ship-of-tools). This document is written for
*you*: follow it top to bottom, adapt where your judgment says so, and keep
the human informed at each step. For the release-artifact path, the
deterministic engine underneath is `scripts/install.sh`; your job is the
judgment around it — choosing the right topology, verifying prerequisites, and
proving the result works.

## Which install is this?

Published GitHub Releases with prebuilt artifacts exist (v0.3.1+, Linux
x86_64 / Windows x86_64 / macOS aarch64), so the default install path is the
**release installer** below. Verify assets for the latest tag exist
(`gh release view --repo kalidke/ship-of-tools` or the Releases page) before
relying on them.

The **source path** is for contributors or when release assets don't match the
target platform: clone the repo to a working directory of the human's choice
and follow https://kalidke.github.io/ship-of-tools/dev/start/setup/ (rustup +
juliaup, `cargo build --release --manifest-path rust/Cargo.toml`, Julia env
instantiation; source builds stamp `-dev` and never self-update). Ask if their
intent is ambiguous.

## 0. Ground rules

- Don't run anything as root; everything installs under `$HOME`
  (`~/.local/share/sot`, `~/.config/sot`, `~/.local/bin`). The installer
  honors `SOT_PREFIX` and `XDG_CONFIG_HOME` overrides — if you set them,
  substitute your prefix in every path below (verify, uninstall, all of it).
- Use the installer's **flags**, not its interactive TTY Q&A — you are the
  interactive layer now.
- Report what you find and decide; ask the human only what you cannot infer
  (primarily: which topology they want, and SSH details for remote setups).
- **Lead with what WILL work for the human's machine and topology** — state
  the working path first, then caveats. A wall of warnings before any plan
  reads as "this doesn't work" to a human who just wants to install.

## 1. Preflight (report findings before proceeding)

```bash
uname -sm                 # Linux x86_64 or macOS arm64 for release artifacts
case "$(uname -s)" in
  Linux)  ldd --version | head -1 ;;   # frontend needs glibc >= 2.35
  Darwin) sw_vers -productVersion ;;   # macOS aarch64 artifact only
esac
command -v git curl tar   # all required; jq required if gh is absent
command -v node npm       # OPTIONAL — math rendering in markdown previews
command -v tmux && tmux -V # REQUIRED on any host that RUNS the backend (local / be-only)
```

- **node/npm absent** → not a blocker: the installer skips the MathJax
  sidecar deps with a warning and math in markdown previews shows raw LaTeX.
  Tell the human; if they want math, install node and re-run (or run
  `npm ci` in `<checkout>/rust/backend/sidecars/mathjax`).
- **tmux** → a **hard backend dependency**: the daemon hosts the LLM pane in a
  tmux session. Any host that runs `sotd` (a `--local` or `--be-only` install)
  needs it; a **frontend-only** `--backend <alias>` host does not (its daemon is
  remote). **Absent → fatal** (the installer now stops with a clear message).
  **tmux < 3.2 → graceful degrade, not an error**: `new-session -e` (used to
  stamp the pane's `SOT_*` awareness env) is a 3.2 flag, and on older tmux (e.g.
  3.0a on Ubuntu 20.04) it was rejected at arg-parse — which historically drove a
  respawn storm that forked ~339k zombie tmux clients (expectations, 2026-07-11).
  The daemon now **version-gates** the flag: on tmux < 3.2 it omits `-e` and falls
  back to a best-effort `set-environment`, so the backend runs but the pane's
  in-session awareness is best-effort only. For full awareness, put a
  **tmux ≥ 3.2** earlier on the daemon's `PATH` (e.g. a user-local build in
  `~/.local/bin`).
- **Linux x86_64, glibc ≥ 2.35** → full install works.
- **Linux, older glibc** → only `--be-only` (the backend is static musl);
  the frontend must run on another machine.
- **Windows** → no bash installer. Build from source, or use **§2b Windows
  frontend → remote backend** only after a Windows release zip exists.
- **macOS (Apple Silicon)** → supported by the installer, same three
  topologies as Linux, but still EXPERIMENTAL until it is dogfooded on real
  Macs; say so, then proceed. `--backend <ssh-alias>` is the common
  want: frontend on the Mac, backend on a Linux box — ONE command, the
  installer writes a tunnel-opening launcher. No systemd on macOS: the
  local-role launcher starts `sotd` on demand instead. Intel Macs:
  from-source only.

## 2. Choose the topology (ask the human, one question)

> "Where should Ship of Tools run? (a) everything on this machine,
> (b) the UI here but the backend on a server you SSH to,
> (c) backend only on this machine (headless server)."

| Answer | Installer flags |
|--------|-----------------|
| (a) all-in-one | `--local` |
| (b) UI here, backend remote | `--backend <ssh-alias>` |
| (c) headless backend | `--be-only` (add `--no-service` for an optional shared-home deployment) |

For (b): verify key-based SSH first — `ssh -o BatchMode=yes <alias> true`.
If it fails, walk the human through `ssh-keygen` + `ssh-copy-id`, then
recheck. The *remote* machine also needs a `--be-only` install (offer to do
it over SSH after this one). **On a shared-home deployment**, always add
`--no-service`: a `systemd --user` unit written into a shared `$HOME` applies to
every host sharing it.

## 2b. Windows frontend → remote backend

Windows is a first-class *frontend* host (the backend stays on Linux). No
bash installer here — four steps (issue #23):

1. **Download + verify** from the selected release
   (https://github.com/kalidke/ship-of-tools/releases):
   `sot-<ver>-windows-x86_64.zip` + `SHA256SUMS`; check the hash
   (`Get-FileHash -Algorithm SHA256`), extract `sot.exe` somewhere stable
   (e.g. `%LOCALAPPDATA%\sot\bin`).
2. **Backend**: install it on the Linux machine (`--be-only`, see the table
   above — you can drive that over SSH). It listens on that user's private
   socket, normally `/run/user/<uid>/sot/sessions/sot.sock`.
3. **Forward the ports** — the protocol port is local-only and terminates at
   the remote socket; the aux ports carry
   Pluto, video, and the docs-site servers (`W` and `o` silently break
   without them):

   ```powershell
   $sock = ssh <ssh-alias> '~/.local/share/sot/bin/sotd session-socket-path sot'
   ssh -N -L "18743:$sock" -L 1234:127.0.0.1:1234 -L 1235:127.0.0.1:1235 `
       -L 1236:127.0.0.1:1236 -L 1237:127.0.0.1:1237 -L 1238:127.0.0.1:1238 `
       -L 1239:127.0.0.1:1239 -L 1240:127.0.0.1:1240 <ssh-alias>
   ```

4. **Launch**: `sot.exe --tcp 127.0.0.1:18743` (`sot.exe --help` prints the
   full flag set). Optionally persist the connection in
   `%APPDATA%\sot\hosts.toml` (config discovery: `$SOT_HOSTS` →
   `<repo>/.sot/hosts.toml` → `%APPDATA%\sot\hosts.toml`).

   `sot.exe` does NOT open the SSH forward itself — the tunnel is yours (or
   a launcher's). The dev repo's `scripts/launch-sot.ps1` automates
   forward + launch + respawn for source checkouts; a packaged `install.ps1`
   is tracked in issue #23.

## 2c. macOS

When macOS release artifacts exist, use the installer — it handles macOS
natively (artifact selection, checksum via `shasum`, Gatekeeper de-quarantine,
launcher):

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh \
  | bash -s -- --backend <ssh-alias>     # FE on this Mac -> BE on a Linux box
  # or: --local (everything on the Mac) / --be-only (headless Mac backend)
```

`--backend` writes a `sot-launch` that opens the SSH forwards (local 18743 to
the remote socket, plus 1234-1240) and starts the frontend; `--local` starts
`sotd` on demand (no systemd on macOS; launchd wiring is roadmap). If the human prefers manual
steps: download `sot-<ver>-macos-aarch64.tar.gz` + `SHA256SUMS`, verify
(`shasum -a 256 -c`), `xattr -d com.apple.quarantine ./sot ./sotd`, forward
the ports as in 2b, `./sot --tcp 127.0.0.1:18743`.

## 3. Install

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh \
  | bash -s -- <FLAGS-FROM-STEP-2>
```

You are RIGHT to be wary of piping remote scripts to a shell — inspecting
first is encouraged: download it to a file, read it (it is ~400 commented
lines, everything under `$HOME`, no sudo, checksums verified before use),
then run the file with the same flags.

Idempotent; re-running is also the upgrade path after release artifacts exist.
(If you fetched this runbook at a pinned commit, still use `main`'s installer as
above — the installer is the moving part and stays compatible with this
runbook.) It downloads the release binaries, verifies SHA256 checksums, clones
the repo at the release tag into
`~/.local/share/sot/repo/current` (blobless — small), installs Julia via
juliaup if missing, installs the agent comm resources with
`ShipTools.update_comm()` (Claude/Codex skills and `~/.sot-comm/bin`), and, for
backend roles, instantiates the Julia environments (`julia/kernel`,
`julia/repl`, and `julia/pluto`; Pluto is also precompiled/loaded for
first-open latency). It writes
`~/.config/sot/hosts.toml` + `settings.toml`
(never clobbering user edits on same-role re-runs), and wires a `sot-launch`
wrapper + desktop entry (frontend roles) or a systemd user unit (backend
roles, unless `--no-service`).

## 4. Verify (do not skip; report results)

```bash
~/.local/share/sot/bin/sotd --version        # must print the release version
git -C ~/.local/share/sot/repo/current describe --tags   # must equal the tag
```

For backend roles, prove the daemon answers a hello. Two branches:

- **Service install** (default): `systemctl --user status sotd` should show
  active — the daemon is already running.
- **`--no-service` install**: no unit exists; boot the daemon yourself
  (the installer's final output also prints a supervise hint):

  ```bash
  ~/.local/share/sot/bin/sotd \
      --project-root ~/.local/share/sot/repo/current --label sot &
  ```

Then probe its socket (success = it prints `backend answers: <the release
version>`):

```bash
sock="$(~/.local/share/sot/bin/sotd session-socket-path sot)"
tmp="$(mktemp "${TMPDIR:-/tmp}/sot-hello.XXXXXX")"
(
  printf '%s\n' \
    '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"install-check","last_seen_revision":0,"protocol":1,"app_version":"agent-install"}}' \
    | nc -U "$sock" > "$tmp"
) &
pid=$!
for _ in 1 2 3 4 5; do [ -s "$tmp" ] && break; sleep 1; done
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
ans="$(sed -n 's/.*"app_version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp")"
rm -f "$tmp"
[ -n "$ans" ] || { echo "ERROR: no hello response from $sock"; exit 1; }
echo "backend answers: $ans"
```

For frontend roles: run `sot-launch`; a native window should open and
connect. Tell the human: **press `?` for help; the top line of the nav pane
always shows the pane-switch keys.**

## 5. Troubleshooting (the known failure modes)

| Symptom | Cause → fix |
|---------|-------------|
| `glibc >= 2.35` error | distro too old for the prebuilt frontend → use `--be-only` here + frontend elsewhere, or build from source |
| checksum verification FAILED | truncated download → re-run; still failing = report, don't bypass |
| `ssh ... doesn't work` during (b) | no key auth → `ssh-copy-id` then re-run |
| GitHub API rate-limit (60/h per IP) | authenticate `gh` or set `$GITHUB_TOKEN` (optional otherwise) |
| dirty-checkout refusal on upgrade | the human edited `repo/current` → `git -C ... stash` (or commit), re-run |
| local port 18743 already bound | another tunnel owns it → `--port <n>` |
| backend socket missing | old TCP-based service unit or failed daemon start → reinstall/restart the socket-based `sotd.service` |
| `tmux is required` at install | backend host has no tmux → install it (`apt install tmux`, or a user-local tmux ≥ 3.2 in `~/.local/bin`) and re-run |
| LLM pane never appears / `sotd.log` spams `pty EOF — respawning tmux` then `cooling down` | tmux < 3.2 on the backend (the daemon degrades gracefully now — no more storm — but check the log's `tmux capability probe` line; put tmux ≥ 3.2 earlier on the daemon's PATH for full awareness) |
| Julia instantiate slow on first run | normal (precompilation); minutes, once |

## 6. After the install

- The checkout **is the manual**: point yourself (and the human) at
  `~/.local/share/sot/repo/current/docs/USING.md` — inside the app, the
  terminal-drawer agent gets the same path via `$SOT_MANUAL`.
- Updating later after release artifacts exist: re-run step 3 (the app also
  notifies about new releases and stages fresh binaries itself).
- Uninstall: `rm -rf ~/.local/share/sot ~/.config/sot ~/.local/bin/sot-launch`
  and `systemctl --user disable --now sotd` if a unit was installed.
