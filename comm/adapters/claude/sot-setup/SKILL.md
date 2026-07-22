---
name: sot-setup
description: One-shot, cross-OS onboarding for a Ship of Tools machine (Windows / Linux / macOS). Walks the user through a Q&A (machine role + backend server info), then does USER-LEVEL installs of every dependency (Rust via rustup, Julia via juliaup when needed), builds the rust/ workspace, writes hosts.toml + settings.toml from the answers, installs the Claude statusline INCLUDING the Windows `bash -c` forward-slash fix, installs + joins sot-comm, and creates a launcher/shortcut. Use for "set up Ship of Tools", "Ship of Tools setup", "onboard this machine", "install the dev env", "new machine setup", "set up rust/julia/statusline for Ship of Tools". Supersedes the older Windows-only setup flow.
---

# sot-setup

Onboard **the machine you are running on** as a Ship of Tools node, on any OS. The
frontend (Rust TUI) runs where there's a display; the backend daemon + Julia
kernel usually run on a remote (e.g. `myhost`) over an SSH-forwarded socket.
Everything installs **user-level** (no system-wide writes) wherever the platform
allows.

> **Canonical copy:** this file (repo `.claude/skills/`). The install payload
> `comm/adapters/claude/sot-setup/SKILL.md` is a byte-for-byte copy —
> edit HERE, then sync the payload and re-run `/sot-install` to close skew.

**Golden rules**
- **Repo is canonical.** Read live values from the repo, never hardcode a user's
  paths. Substitute the real `$HOME`/`%USERPROFILE%` and repo root you detect.
- **User-level installs.** rustup (`~/.cargo`, `~/.rustup`), juliaup
  (`~/.juliaup`). The one unavoidable exception is the Windows **MSVC** linker
  (VS Build Tools) — offer it, and fall back to the GNU toolchain if admin is
  unavailable (§2a).
- **Confirm before each install step.** Print what will be installed and where.
- **Idempotent.** Detect what's already present and skip it. Re-running is safe.

---

## 0. Q&A — gather role + server info (do this FIRST, with `AskUserQuestion`)

Detect the OS yourself (don't ask): `win32` / `linux` / `darwin`. Then ask the
user only what you can't derive. Use `AskUserQuestion` with these:

1. **Machine role** — what is this box?
   - *Frontend client* (default): runs the TUI locally, connects to a remote
     backend over SSH. Needs Rust + build; **no local Julia**.
   - *Backend server*: headless host for the daemon + Julia kernel (e.g. a Linux
     remote). Needs Rust + **Julia** + tmux; no display.
   - *All-in-one (local backend)*: both on this machine (local socket/pipe, no SSH).

2. **Backend server connection** (only if role = Frontend client). Free-text /
   confirm defaults:
   - SSH alias or `user@host` (default `myserver`)
   - Remote repo path (default `/home/<user>/ship-of-tools`)
   - Backend TCP port (default `18743`)
   - Auth token (default none / open mode)
   - **Provision the remote BE over this SSH?** (default yes, unless it's already
     built) — a frontend client *must* have working SSH to the BE anyway (the
     launcher tunnels + spawns `sotd` over it), so one run can set up both ends
     instead of a separate hands-on pass on the Linux box. See §6b.

3. **sot-comm handle** — short stable name for this session
   (e.g. `win-fe`, `mac-fe`, `myhost-be`). Default `<repo>-<host>`.

4. **Statusline?** (yes/no, default yes) — install the 2-line Claude statusline.

Echo the collected answers back as a short plan before proceeding.

---

## 1. Prerequisites + preflight (all OSes)

**Preflight (run these first; they decide what's even possible):**
- **Display, if this box should run the FE** (Linux/mac): the frontend opens a GPU
  window — it needs a display. Check:
  `[ -n "$DISPLAY$WAYLAND_DISPLAY" ] && echo "FE OK" || echo "HEADLESS → backend-only"`.
  If headless, this machine is **backend-only**; the FE belongs on a box with a
  display. (macOS always has one.)
- **SSH to the backend host, if role = frontend client** (remote BE): confirm
  `ssh -o BatchMode=yes <host> true` succeeds *without a password*. If it prompts
  or fails, set it up first: `ssh-keygen -t ed25519` (if no key) →
  `ssh-copy-id <host>` → add a `Host` alias to `~/.ssh/config`. The launcher
  assumes non-interactive SSH (`-fN` tunnel + remote spawn); a missing key is the
  #1 reason the FE can't reach the BE.

**Tools:**
- **git** — `git --version`. If absent: Windows `winget install --id Git.Git -e`;
  macOS `xcode-select --install` (provides git); Linux use the system pkg mgr
  (this is the one place sudo may be needed) or skip if already present.
- A C compiler / linker for Rust native build deps (tree-sitter `build.rs`):
  - Windows → MSVC or GNU (see §2a)
  - macOS → Xcode Command Line Tools: `xcode-select --install` (user-prompted)
  - Linux → `cc`/`gcc` (usually present; else system pkg)
- **`jq` + `awk`** for the Linux/mac statusline (§8b) — usually present; `jq` may
  need a user-level/system install.

---

## 2. Install Rust (user-level, all OSes)

If `cargo --version` already works, skip. Otherwise:

### 2a. Windows
Prefer MSVC (what CI uses):
```powershell
winget install --id Rustlang.Rustup --exact --silent --accept-source-agreements --accept-package-agreements
# C/C++ linker (admin prompt — this is the only non-user-level step):
winget install --id Microsoft.VisualStudio.2022.BuildTools --exact --silent `
  --accept-source-agreements --accept-package-agreements `
  --override "--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --add Microsoft.VisualStudio.Component.Windows11SDK.22621"
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
rustc --version; cargo --version; rustup show active-toolchain   # expect *-windows-msvc
```
**If admin/VS Build Tools is unavailable**, go fully user-level with the GNU toolchain:
```powershell
winget install --id Rustlang.Rustup -e --silent --accept-source-agreements --accept-package-agreements
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup default stable-x86_64-pc-windows-gnu
# GNU needs a mingw-w64 gcc on PATH; user-level option:
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e   # or any mingw-w64; add its bin/ to PATH
```

### 2b. Linux / macOS
```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
. "$HOME/.cargo/env"
rustc --version; cargo --version
```
**Linux frontend client only** also needs Vulkan + graphics dev libs for `wgpu`
(this needs the system pkg mgr / sudo — flag it explicitly to the user; a headless
backend server does NOT need these):
- Debian/Ubuntu: `sudo apt install -y libvulkan1 mesa-vulkan-drivers libxkbcommon-x11-0 libxkbcommon-dev libwayland-dev libxcb1 pkg-config`
  (Fedora: `vulkan-loader mesa-vulkan-drivers libxkbcommon-devel wayland-devel`.)
  Exact set varies by distro / X11 vs Wayland; if the **build** fails it names a
  missing `*-dev`/`pkg-config` package, and if the build is fine but the FE
  **panics at window/surface creation at runtime**, the Vulkan ICD/loader is
  missing (`vulkaninfo` to confirm a working driver).
- macOS: nothing (Metal is built in).

---

## 3. Install Julia (only if role = backend server or all-in-one)

Frontend-client machines do **not** need Julia. When needed, juliaup (user-level):
- Windows: `winget install --id Julialang.Juliaup -e --silent --accept-source-agreements --accept-package-agreements`
- Linux/macOS: `curl -fsSL https://install.julialang.org | sh -s -- --yes`

Then `juliaup add release` (or pin `1.11+`, per `julia/kernel/Project.toml` compat),
and verify `julia --version`.

---

## 4. Get the repo (if not already cloned)

If you're running inside the repo, skip. Otherwise clone to a stable path
(mirror the existing layout if the user has one):
```sh
git clone https://github.com/kalidke/ship-of-tools.git <dest>
```
Record the absolute repo root as `$REPO` for the rest of this skill.

---

## 5. Build the Rust workspace

From `$REPO/rust` (Windows: ensure `~\.cargo\bin` is on PATH first):
```sh
cargo build --release                       # both binaries
# frontend-client box:   cargo build --release -p sot-frontend
# headless backend-only:  cargo build --release -p sot-backend
```
Outputs:
- `$REPO/rust/target/release/sot[.exe]`    (~19 MB)
- `$REPO/rust/target/release/sotd[.exe]`   (~3 MB)

First build is slow (graphics + tree-sitter grammars). Tee the log so failures are
inspectable: Windows `... 2>&1 | Tee-Object $env:TEMP\sot-cargo-build.log`.

---

## 6. Julia kernel deps (backend server / all-in-one only)

The kernel runs as `julia --project=julia/kernel`. Its local path-deps (`core` +
the seven doc/preview plugins + `HDF5Preview`) are declared in
`julia/kernel/Project.toml`'s **`[sources]`** section, with paths **relative to
that file** — so a fresh clone bakes no absolute paths and needs no
`Pkg.develop`, just instantiate (the `Manifest.toml` stays gitignored and is
regenerated, now with relative paths too). This needs **Julia >= 1.11** (the
kernel's `[compat]` enforces it; on 1.10 `[sources]` is silently ignored and the
kernel can't find its deps — the post-rename "Missing source file" crash this
prevents):
```sh
julia --project=julia/kernel -e 'using Pkg; Pkg.instantiate()'
julia --project=julia/kernel -e 'using ShipToolsKernel'   # sanity: must load clean
```
Two more Julia envs the backend lane touches — instantiate them too:
- **Root project** (`$REPO` = `ShipTools`, needed by `ShipTools.update_comm()`
  in §9): `julia --project=. -e 'using Pkg; Pkg.instantiate()'`
- **REPL env** (`julia/repl` = `ShipToolsRepl`, the persistent REPL the backend
  supervises; registry-only deps, no path-deps):
  `julia --project=julia/repl -e 'using Pkg; Pkg.instantiate()'`

The sanity `using ShipToolsKernel` is the real gate — if it loads clean, the
kernel env is wired. (No `develop` step can drift or miss a plugin now: the
`[sources]` list in `Project.toml` is the single source of truth.)

---

## 6b. Remote backend provisioning over SSH (frontend-client role)

A frontend client **must** have working non-interactive SSH to the backend host
(§1 preflight) — the launcher tunnels + spawns `sotd` over it on every run. Since
that SSH is non-negotiable, use it to provision the BE too, instead of a separate
hands-on pass on the Linux box. **Offer this whenever role = frontend client and
the remote isn't already built** — probe first, skip if it is (idempotent):
```sh
ssh <host> "test -x '<remote_repo>/rust/target/release/sotd'" && echo BUILT || echo NEEDS-PROVISION
```
Run every command below from the **FE box's own shell** (git-bash on Windows, any
POSIX shell on Linux/mac) — they only use `ssh`. Everything the BE needs is
user-level and scriptable; the only exceptions are a few system packages that may
need `sudo` (below) — detect and report those, never auto-sudo.

**Gotcha — non-interactive SSH shells are non-login:** they don't source
`~/.profile`, so `~/.cargo/bin` and `~/.juliaup/bin` are NOT on PATH even right
after the installers run. Every remote step must bring the env in itself
(`. "$HOME/.cargo/env"`; `export PATH="$HOME/.juliaup/bin:$PATH"`).

Probe what's present + what needs sudo:
```sh
HOST=myhost; REMOTE_REPO=/home/<user>/ship-of-tools
ssh "$HOST" 'for t in git tmux cc cargo julia; do printf "%s: " "$t"; command -v "$t" || echo MISSING; done'
```
- `git` / `tmux` / `cc` **MISSING** → system packages (sudo). Stop and ask the user
  to install them (`sudo apt install -y git tmux build-essential`, or the distro
  equivalent). A **headless** BE needs **no** graphics/Vulkan libs (that set is
  FE-only, §2b).
- `cargo` / `julia` MISSING → user-level; install below.

Provision (each block idempotent; run only the missing ones):
```sh
# Rust (user-level)
ssh "$HOST" 'command -v cargo >/dev/null || curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path'
# Julia via juliaup (user-level)
ssh "$HOST" 'command -v julia >/dev/null || curl -fsSL https://install.julialang.org | sh -s -- --yes'
ssh "$HOST" 'export PATH="$HOME/.juliaup/bin:$PATH"; juliaup add release'
# Repo — clone or fast-forward
ssh "$HOST" "test -d '$REMOTE_REPO/.git' && git -C '$REMOTE_REPO' pull --ff-only || git clone https://github.com/kalidke/ship-of-tools.git '$REMOTE_REPO'"
```

Build `sotd` — **backgrounded + logged**, then poll, so an SSH drop doesn't kill a
multi-minute build:
```sh
ssh "$HOST" ". \$HOME/.cargo/env; cd '$REMOTE_REPO/rust' && nohup cargo build --release -p sot-backend >/tmp/sot-be-build.log 2>&1 & echo build-pid=\$!"
ssh "$HOST" "while pgrep -x cargo >/dev/null; do sleep 5; done; ls -l '$REMOTE_REPO/rust/target/release/sotd' && tail -3 /tmp/sot-be-build.log"
```

Julia envs (kernel + root + repl); needs Julia ≥1.11 (§6). The `using
ShipToolsKernel` load is the real gate:
```sh
ssh "$HOST" "export PATH=\"\$HOME/.juliaup/bin:\$PATH\"; cd '$REMOTE_REPO' && \
  julia --project=julia/kernel -e 'using Pkg; Pkg.instantiate()' && \
  julia --project=. -e 'using Pkg; Pkg.instantiate()' && \
  julia --project=julia/repl -e 'using Pkg; Pkg.instantiate()' && \
  julia --project=julia/kernel -e 'using ShipToolsKernel; println(\"KERNEL_OK\")'"
```
`KERNEL_OK` = BE fully provisioned; the FE launcher's remote-spawn (§10) drives
`sotd` from here on. (Backend-server / all-in-one roles run these same steps
**locally** as §2/§3/§5/§6 — this section is only the "drive them over the FE's
SSH" variant.)

---

## 7. Configure hosts.toml + settings.toml (from the Q&A)

**hosts.toml** — write the chosen backend into the frontend's host registry.
Discovery order: `$SOT_HOSTS` → `$REPO/.sot/hosts.toml` → per-user config
(`%APPDATA%\sot\hosts.toml` on Windows, `~/.config/sot/hosts.toml` on
Linux/mac). Prefer the per-user config so it survives repo updates:
```toml
default_host = "myserver"

[host.myserver]                       # name = the SSH alias / friendly label
ssh_alias   = "myserver"             # or user@host
remote_repo = "/home/<user>/ship-of-tools"
tcp_port    = 18743
remote_home = "/home/<user>"

# all-in-one / local backend instead of SSH:
# [host.local]
# socket = "\\.\pipe\sot-local"   # Windows named pipe; Linux: a unix socket path
```

**settings.toml** (optional but recommended) — `$REPO/.sot/settings.toml` or
`~/.config/sot/settings.toml`. Most important key for the dogfood loop is the
ADR-0017 resume command:
```toml
[terminal]
resume_command = "claude --dangerously-skip-permissions --continue /sot-fe-session-start"
# shell = "..."   # optional override; default auto-resolves (pwsh→powershell→cmd / $SHELL→bash→sh)

[layout]
preset = "auto"   # auto | ultrawide | laptop | portrait
```

---

## 8. Statusline (incl. the Windows `bash -c` fix)

Claude Code runs the configured statusLine command **through a shell**: when
`process.env.SHELL` is set it runs `SHELL -c "<command>"` (and on Windows it will
even auto-discover git-bash and set `SHELL` itself). **Consequence:** inside the
Ship of Tools Terminal drawer (which inherits `SHELL=/bin/bash.exe` from a MINGW64 launch
chain) a Windows command path with **backslashes** gets mangled by bash
(`C:\Users\...` → `C:Users...`, exit 127) and the statusLine **silently never
renders** — no error, no output, no footer line. The fix is to use **forward
slashes** in the configured command, which survive both `bash -c` and `cmd /c`.

### 8a. Windows — write the scripts to `~/.claude/`, then configure with FORWARD slashes

Write `%USERPROFILE%\.claude\statusline.bat` (thin wrapper):
```bat
@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0statusline.ps1"
```
Write `%USERPROFILE%\.claude\statusline.ps1` (ASCII-only literals — Windows
PowerShell 5.1 reads .ps1 as CP1252; non-ASCII in a string breaks the parse):
```powershell
# Claude Code statusLine (Windows) - 2 lines, single PowerShell pass.
$ErrorActionPreference = 'SilentlyContinue'
$esc = [char]27
function Col([string]$c,[string]$t){ "$esc[${c}m$t$esc[0m" }
function Fmt([int64]$n){ if($n -ge 1000){ "$([math]::Floor($n/1000))k" } else { "$n" } }
$j = $null; try { $j = [Console]::In.ReadToEnd() | ConvertFrom-Json } catch {}
$model = if ($j.model.display_name) { [string]$j.model.display_name } else { 'model?' }
$sid = [string]$j.session_id; $sess = if ($sid.Length -ge 8) { $sid.Substring(0,8) } else { $sid }
$effort = $j.effort.level
$think = if ($j.thinking.enabled -ne $true) { 'off' } elseif ($effort) { [string]$effort } else { 'on' }
$thinkSeg = if ($think -eq 'off') { Col '90' "think:$think" } else { Col '35' "think:$think" }
$ver = if ($j.version) { Col '33' "v$($j.version)" } else { '' }
$cur = if ($j.workspace.current_dir) { [string]$j.workspace.current_dir } else { '.' }
Push-Location $cur 2>$null
$branch = git branch --show-current 2>$null
if ($branch) { $repo = "$(Split-Path $cur -Leaf):$branch"; $unc = @(git status --porcelain 2>$null).Count }
else { $repo = 'no-git'; $unc = 0 }
Pop-Location 2>$null
$uncCol = if ($unc -eq 0) { '32' } else { '31' }
$l1 = @((Col '34' $model) + ' ' + (Col '90' "[$sess]"), $thinkSeg)
if ($ver) { $l1 += $ver }
$l1 += (Col '38;5;208' $repo); $l1 += (Col $uncCol "$unc uncommitted")
$inT = [int64]$j.context_window.total_input_tokens; $outT = [int64]$j.context_window.total_output_tokens
$cost = [double]$j.cost.total_cost_usd
$costCol = if ($cost -gt 0.50) { '31' } elseif ($cost -gt 0.10) { '33' } else { '32' }
$l2 = (Col '36' "Session: $(Fmt ($inT+$outT)) (in:$(Fmt $inT) out:$(Fmt $outT))") + ' | ' + (Col $costCol ('${0:N2}' -f $cost))
[Console]::Out.Write([string]::Join(' | ', $l1) + "`n" + $l2)
```
Then set the command in `%USERPROFILE%\.claude\settings.json` — **forward slashes**:
```json
{ "statusLine": { "type": "command", "command": "C:/Users/<you>/.claude/statusline.bat" } }
```
Merge into existing settings.json (preserve other keys). CC hot-reloads it — no
restart needed.

### 8b. Linux / macOS — `~/.claude/statusline.sh`
Full parity with the Windows `statusline.ps1` (same 2-line format + colors).
**No transcript-model read** — CC's per-session JSON already carries the right
model, so the old `statusline-session-model.sh` transcript-grep workaround is
obsolete (and was its main cost). Requires `jq` + `awk` (both ubiquitous).
```sh
#!/usr/bin/env bash
# Claude Code statusLine (Linux/macOS) - mirrors the Windows statusline.ps1.
j="$(cat)"; e=$'\033'
col(){ printf '%s[%sm%s%s[0m' "$e" "$1" "$2" "$e"; }
get(){ printf '%s' "$j" | jq -r "$1 // empty" 2>/dev/null; }
fmt(){ local n=${1:-0}; if [ "$n" -ge 1000 ] 2>/dev/null; then echo "$((n/1000))k"; else echo "$n"; fi; }
model="$(get .model.display_name)"; [ -z "$model" ] && model='model?'
sid="$(get .session_id)"; sess="${sid:0:8}"; ver="$(get .version)"
eff="$(get .effort.level)"; th="$(get .thinking.enabled)"
if [ "$th" != "true" ]; then think=off; tc=90; elif [ -n "$eff" ]; then think="$eff"; tc=35; else think=on; tc=35; fi
cur="$(get .workspace.current_dir)"; [ -z "$cur" ] && cur=.
br="$(git -C "$cur" branch --show-current 2>/dev/null)"
if [ -n "$br" ]; then repo="$(basename "$cur"):$br"; unc="$(git -C "$cur" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"; else repo=no-git; unc=0; fi
[ "${unc:-0}" -eq 0 ] && uc=32 || uc=31
inT="$(get .context_window.total_input_tokens)"; outT="$(get .context_window.total_output_tokens)"
cost="$(get .cost.total_cost_usd)"; cost=${cost:-0}
cc=32; awk "BEGIN{exit !($cost>0.10)}" && cc=33; awk "BEGIN{exit !($cost>0.50)}" && cc=31
l1="$(col 34 "$model") $(col 90 "[$sess]") | $(col "$tc" "think:$think")"
[ -n "$ver" ] && l1="$l1 | $(col 33 "v$ver")"
l1="$l1 | $(col '38;5;208' "$repo") | $(col "$uc" "$unc uncommitted")"
printf '%s\n%s | %s\n' "$l1" \
  "$(col 36 "Session: $(fmt $(( ${inT:-0} + ${outT:-0} ))) (in:$(fmt ${inT:-0}) out:$(fmt ${outT:-0}))")" \
  "$(col "$cc" "$(printf '$%.2f' "$cost")")"
```
`chmod +x ~/.claude/statusline.sh`; settings.json command = the **absolute
path** `"/home/<you>/.claude/statusline.sh"` (the §8 forward-slash gotcha is
Windows-only — POSIX paths are native, no `bash -c` mangling). **On an optional
shared-home deployment** the maintained `statusline-session-model.sh` may already
exist at `~/.claude/` and settings.json there may point at it — leave that in
place; write this stub only on a fresh machine that needs it.

### 8c. Verify the statusline actually fires
Pipe sample JSON and confirm it prints two colored lines, then confirm CC invokes
it (temporarily add a debug line that appends `$raw` to a log, watch for a real
session id — full UUID, not a hand-typed test value — then remove it):
```sh
echo '{"model":{"display_name":"Opus 4.8"},"session_id":"abc","version":"x","workspace":{"current_dir":"'$PWD'"},"context_window":{"total_input_tokens":120000,"total_output_tokens":34000},"cost":{"total_cost_usd":0.42}}' \
  | bash -c "<the exact command from settings.json>"     # must exit 0 and print 2 lines
```
If it works under `bash -c` with the configured path, it'll work in the drawer.

---

## 9. sot-comm (session messaging)

Install the comm scripts + Claude skill adapters (idempotent), then join:
```sh
julia --project=. -e 'using ShipTools; ShipTools.update_comm()'   # from $REPO; copies to ~/.sot-comm/bin + ~/.claude/skills
~/.sot-comm/bin/comm-join.sh --name <handle>             # registers in ~/.sot-comm/registry.json
~/.sot-comm/bin/comm-list.sh                             # verify the handle shows up
```
**`comm-join.sh` takes only `--name` (and optional `--expertise "a, b"`)** — there
is no `--endpoint` flag; it registers this session in the local file registry.
Cross-machine messaging rides the relay over the backend tunnel at runtime, not a
join-time endpoint.

**No-Julia fallback (frontend-client machines):** `ShipTools.update_comm()` needs
Julia, which FE clients don't install (§3). `update_comm()` is just a file copy —
replicate it directly (idempotent), then join as above:
```sh
mkdir -p ~/.sot-comm/bin
cp -f "$REPO"/comm/core/scripts/* ~/.sot-comm/bin/ && chmod 755 ~/.sot-comm/bin/*.sh
for s in sot-comm sot-install; do
  mkdir -p ~/.claude/skills/$s
  cp -f "$REPO/comm/adapters/claude/$s/SKILL.md" ~/.claude/skills/$s/SKILL.md
done
```
Note this XDG `$HOME` may be **per-machine**, so its registry is local to the
box; the relay still bridges sessions across machines over the tunnel.

(Windows: the FE writes inbound relay messages to
`%LOCALAPPDATA%\sot\fe-inbox.jsonl`; the in-drawer session watches that file
via a Monitor — `/sot-fe-session-start` re-arms it on each relaunch.) Installing
new skills requires a Claude Code **restart** to load them.

---

## 10. Launcher / shortcut + first run

### Windows
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File "$REPO\scripts\install-shortcut.ps1"
```
Creates `Desktop\Ship of Tools.lnk` → `scripts\launch-sot.ps1`, which: reads
`last_host` from `%APPDATA%\sot\state-<COMPUTERNAME>.toml` (resolved against
hosts.toml), opens the SSH tunnel (`-L 18743`, `-L 1234` Pluto, `-L 1235` video),
spawns/refreshes the remote `sotd --tcp 127.0.0.1:18743`, then runs the
staged frontend with the ADR-0017 supervisor loop (exit-75 relaunch). Launch it
and confirm Files mode renders.

> **Display required for the FE.** The frontend opens a real GPU window
> (wgpu + winit). On a **headless** Linux box (`$DISPLAY`/`$WAYLAND_DISPLAY`
> unset) it cannot render — that machine is **backend-only** (run the BE, point a
> frontend on a machine *with* a display at it). Don't try to X-forward the FE; a
> wgpu app over X11-forwarding won't work usefully. Check first:
> `[ -n "$DISPLAY$WAYLAND_DISPLAY" ] && echo "has display" || echo "HEADLESS — BE only"`.

### Linux / macOS — all-in-one (local backend, no SSH) — simplest first run
No checked-in launcher exists, so generate one. This is the lowest-friction path
on a Linux **desktop** / head machine with a display: BE + FE local over a unix
socket. Write `$REPO/scripts/launch-sot-local.sh`:
```sh
#!/usr/bin/env bash
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SOCK="${SOT_SOCKET:-/tmp/sot-$USER.sock}"
rm -f "$SOCK"
"$REPO/rust/target/release/sotd" --socket "$SOCK" --project-root "$REPO" \
    >/tmp/sotd.log 2>&1 &
BE=$!; trap 'kill $BE 2>/dev/null' EXIT
for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.1; done   # wait for socket
[ -S "$SOCK" ] || { echo "backend never opened $SOCK — see /tmp/sotd.log"; exit 1; }
exec "$REPO/rust/target/release/sot" --socket "$SOCK"
```
`chmod +x`. Run it; confirm Files mode renders and the REPL drawer (Ctrl+J) gets a
live Julia. If the FE errors on the socket, check `/tmp/sotd.log` (kernel
env not built? → §6).

### Linux / macOS — frontend client → remote backend (SSH)
For connecting to a remote BE (e.g. a remote host) instead of a local one. Requires working
SSH to the host (see §1 preflight). Write `$REPO/scripts/launch-sot.sh`:
```sh
#!/usr/bin/env bash
set -uo pipefail   # NOT -e: a re-run must not abort just because the tunnel exists
REPO="$(cd "$(dirname "$0")/.." && pwd)"
HOST="${SOT_HOST:-myhost}"; PORT="${SOT_TCP_PORT:-18743}"
REMOTE_REPO="${SOT_REMOTE_REPO:-/home/$USER/ship-of-tools}"
port_open(){ (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null && exec 3>&-; }
# Tunnel: `ssh -fN` is backgrounded and OUTLIVES the FE window, so a second run
# would collide on the bound ports and (under set -e) abort before the FE. Reuse.
if port_open "$PORT"; then echo "reusing existing tunnel on $PORT"; else
  ssh -fN -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes \
      -L "$PORT:127.0.0.1:$PORT" -L 1234:127.0.0.1:1234 -L 1235:127.0.0.1:1235 "$HOST" \
    || { echo "tunnel failed (stale tunnel? pkill -f 'ssh -fN.*$PORT')" >&2; exit 1; }
fi
# Backend: spawn only if not already up (don't disrupt a live session).
if [ "${SOT_RESTART_BE:-0}" = 1 ] || ! ssh "$HOST" 'pgrep -x sotd >/dev/null 2>&1'; then
  ssh "$HOST" "pkill -x sotd 2>/dev/null; sleep 0.3; cd '$REMOTE_REPO' && nohup ./rust/target/release/sotd --tcp 127.0.0.1:$PORT --project-root '$REMOTE_REPO' >/tmp/sotd.log 2>&1 </dev/null & disown" || true
  for _ in $(seq 1 40); do port_open "$PORT" && break; sleep 0.25; done
fi
exec "$REPO/rust/target/release/sot" --tcp "127.0.0.1:$PORT"
```
`chmod +x`. (The remote BE must already be **built** on the host — if it's a fresh
host, provision it over this same SSH first (§6b), or run §1–6 on the box
directly.) **Idempotency matters:** the backgrounded `ssh -fN`
tunnel persists after the FE window closes, so the launcher must reuse it (and not
`set -e`-abort on the bind collision) or it only ever works once.

### Linux desktop launcher (GNOME/freedesktop) — pin to the dash/sidebar
To get a clickable icon in the activities grid + dash (the Windows
`install-shortcut.ps1` equivalent), drop a committed SVG icon + a freedesktop
`.desktop` entry, then pin it. Works for either launcher above — point `Exec` at
the one this box uses (`launch-sot.sh` for SSH, `launch-sot-local.sh` for
all-in-one). A committed `$REPO/scripts/sot.svg` keeps the icon portable.
```sh
cat > ~/.local/share/applications/sot.desktop <<EOF
[Desktop Entry]
Type=Application
Name=Ship of Tools
GenericName=Concept Explorer
Comment=Ship of Tools frontend
Exec=$REPO/scripts/launch-sot.sh
Icon=$REPO/scripts/sot.svg
Terminal=false
Categories=Development;IDE;
Keywords=sot;julia;rust;concept;explorer;
StartupNotify=true
EOF
desktop-file-validate ~/.local/share/applications/sot.desktop   # must print nothing
update-desktop-database ~/.local/share/applications 2>/dev/null
# pin to the GNOME dash (append, idempotent — skip if already present):
cur="$(gsettings get org.gnome.shell favorite-apps)"
echo "$cur" | grep -q sot.desktop || \
  gsettings set org.gnome.shell favorite-apps "$(echo "$cur" | sed "s/]$/, 'sot.desktop']/")"
```
`Terminal=false` because the FE is a GPU window, not a console app. New `.desktop`
entries appear immediately; if not, reload GNOME shell (X11: Alt+F2 → `r`; Wayland:
log out/in). On KDE/other DEs the same `.desktop` file works — pin via the panel's
own "add to favorites" instead of `gsettings`.

### Windows all-in-one
```
sotd --socket \\.\pipe\sot-local --project-root <REPO>
sot --socket \\.\pipe\sot-local
```

---

## 11. Final checklist (report to the user)
- [ ] `cargo --version` (and `julia --version` if backend) resolve to user-level installs
- [ ] `sot` + `sotd` built
- [ ] (frontend-client) remote BE provisioned + `sotd` built over SSH (§6b), or confirmed already built (`KERNEL_OK`)
- [ ] hosts.toml / settings.toml written with the Q&A answers
- [ ] statusline scripts in `~/.claude/`, settings.json command uses **forward slashes**, prints 2 lines under `bash -c`
- [ ] sot-comm installed + joined (`comm-list.sh` shows this handle)
- [ ] launcher/shortcut created; first launch renders Files mode
- [ ] (Windows) taskbar button shows the SoT wheel icon and the running window
      merges into the shortcut/pin button (AUMID `ShipOfTools.Sot` on both the
      `.lnk` and the process — `install-shortcut.ps1` stamps it). Stale generic
      icon after a re-pin → restart Explorer (Windows caches pinned-icon
      metadata; see `scripts/set-shortcut-aumid.ps1`).
- [ ] statusline visible in the Terminal drawer

---

## Appendix — gotchas (hard-won)
- **Statusline blank in the drawer** → backslash path under `bash -c`; use forward
  slashes (§8). Same trap applies to **any hook** with a Windows path — the drawer
  runs all configured commands via `bash -c`.
- **`.ps1` parse errors** → keep string literals ASCII-only (Windows PS 5.1 = CP1252).
- **`python` resolves to the Microsoft Store stub** → shadow it with a shim in a
  PATH dir that precedes `WindowsApps` (e.g. `~/bin`), or `conda init`/juliaup-style
  user installs that prepend their bin.
- **Frontend restart** = ADR-0017 `scripts/relaunch-sot.ps1` (build → exit-75
  sentinel → respawn). NEVER kill the FE process — the dev `claude` runs *inside*
  its Terminal drawer. Supervisor-script changes need a full restart, not exit-75.
- **Bash-tool shells are non-login** (no profile sourced) — PATH changes via
  `~/.bash_profile`/registry won't take effect mid-session; shims in an
  already-on-PATH dir do.
