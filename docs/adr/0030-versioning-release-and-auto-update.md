# ADR 0030: Versioning, releases, and auto-update — going public

**Status:** Accepted (design approved 2026-07-01: publish this repo; Windows+Linux
x86_64 at launch with macOS experimental; update default = notify in-session + auto-apply at
next launch; remote-BE-over-SSH is the design — all-in-one uses the same SSH path to
localhost, SSH key auth to the BE host is a hard requirement)
**Date:** 2026-07-01

## Context

Ship of Tools is going public: other users should be able to install prebuilt binaries and
receive automatic updates. Today none of the machinery for that exists, and the repo carries
per-machine dev state that a public repo must not.

Current state (verified 2026-07-01):

- **Versions are placeholders that have never moved.** Rust workspace `0.1.0`
  (`rust/Cargo.toml [workspace.package]`, inherited by `sot-protocol` / `sot-backend` /
  `sot-frontend`); ShipTools `1.0.0-DEV`; kernel/repl/core `0.1.0`. No binary embeds its
  version — `env!("CARGO_PKG_VERSION")` is unused; there is no `sot --version`.
- **The FE↔BE handshake carries no version.** `HelloReq`/`HelloRes`
  (`rust/protocol/src/ops.rs`) have no version field; `Frame.v` is stamped with
  `PROTOCOL_VERSION = 1` (`rust/protocol/src/lib.rs`) but never validated by the codec.
  Skew surfaces as `"frame parse failed"` or a misbehaving op — behavioral breakage, not a
  clean "please update".
- **Three uncoupled hand-maintained `= 1` constants**: Rust wire `PROTOCOL_VERSION`, Julia
  kernel/repl `PROTOCOL_VERSION` (`julia/kernel/src/ShipToolsKernel.jl`,
  `julia/repl/src/ShipToolsRepl.jl` — the kernel hello even hardcodes `version => "0.1.0"`),
  and `COMM_PROTOCOL_VERSION` (`src/comm.jl`).
- **No tags, no changelog, no Rust CI.** `.github/workflows/CI.yml` is Julia-only; TagBot is
  scaffolded but unused; `git tag -l` is empty. "Update" today = `git pull` + cargo rebuild +
  `systemctl --user restart sotd` / exit-75 relaunch + `ShipTools.update_comm()` file copy.
- **The apply substrate already exists.** ADR 0017's supervisor stages the FE binary
  (`%LOCALAPPDATA%\sot\bin`) and respawns on exit 75; `sotd` runs under `systemd --user`
  with `Restart=always` (ADR 0028); the BE spawns the Julia kernel from a path it controls
  (`rust/backend/src/kernel.rs`). An updater only has to put new bits where these
  mechanisms already look.
- **Per-machine/dev state is committed**: `.sot/hosts.toml` (real hostnames + ssh aliases),
  `.sot/settings.toml`, `.sot/keybindings.toml`, `.claude-bus/` (cross-OS Claude message
  logs), `.claude-memory/` (33 Claude context files). This is a design flaw for a public
  repo: user/machine configuration and dev-fleet coordination don't belong in the product's
  git history going forward.

## Decision

### 1. One product version

Semver `X.Y.Z`. **Single source of truth: `[workspace.package].version` in
`rust/Cargo.toml`.** FE, BE, protocol crate, and the Julia bundle are one product, released
as a unit — no independent component versions, no compatibility matrix. A release script
stamps the Julia `Project.toml`s from the workspace version and creates the git tag
`vX.Y.Z`.

Binaries embed the version via a small `build.rs` (`git describe` + `CARGO_PKG_VERSION`):
`sot --version` / `sotd --version` print `X.Y.Z (<sha> <date>)`. Builds not exactly on a
tag are stamped **`X.Y.Z-dev+<sha>`** — the `-dev` marker is also the hard guard that keeps
the dev fleet out of the auto-updater (below). The kernel hello reports the real embedded
package version instead of a hardcoded string.

Start pre-1.0: first public tag `v0.2.0`. Pre-1.0 semver (minor = anything may change)
until the public install story has soaked.

### 2. Protocol version gate in the handshake

Wire-contract versions stay **separate integers** from the product version — they version
the contract, not the code. Changes:

- `HelloReq` and `HelloRes` gain `protocol: u32` and `app_version: String`
  (`#[serde(default)]` so an old peer deserializes to `0` / `""` and is treated as
  pre-versioning).
- The BE gates the handshake on **protocol integer equality**. Mismatch returns a
  structured error carrying both protocol and product versions; the FE renders an
  "update needed" screen instead of failing on a later op.
- The existing serde-default discipline remains the rule for **additive** changes within a
  protocol version. The integer bumps only on breaking wire changes.
- Same treatment BE↔kernel: the BE validates the kernel's `PROTOCOL_VERSION` at kernel
  hello and fails loud. (In practice BE + Julia bundle ship as a unit, so this is a
  belt-and-suspenders check.)
- `Frame.v` stays stamped as today; the Hello gate makes per-frame validation redundant.
- `COMM_PROTOCOL_VERSION` (sot-comm) is dev-fleet internal and out of scope here.

### 3. Release unit and CI pipeline

One git tag → one GitHub Release containing:

- `sot-<target>` and `sotd-<target>` archives per platform. Matrix at launch:
  **windows-x86_64 and linux-x86_64 (blocking), macos-aarch64 (experimental,
  non-blocking)**. Both binaries build for every platform (Windows all-in-one needs
  `sotd.exe`; same workspace, marginal cost).
  *(Amended 2026-07-09: macOS is now **blocking** too — `release.yml` sets
  `experimental: false` on all three legs and `publish` hard-requires
  `smoke-macos`, so releases cannot silently omit the macOS artifact. The
  platform remains product-experimental in user-facing docs until dogfooded,
  but a macOS build/smoke failure fails the release.)*
- `julia-bundle-vX.Y.Z.tar.gz` — the `julia/` tree (kernel, repl, plugins, pluto) + the
  ShipTools root package, **with `Manifest.toml`s generated and tested in CI**. Manifests
  stay gitignored for dev flexibility, but a release ships a frozen, tested dependency
  set — "instantiate whatever resolves today" is not a release. **Julia requirement:
  1.12+** (2026-07-02) — every `Project.toml` declares compat `julia = "1.12"`,
  which per Julia's caret semantics means ≥1.12, <2.0: a floor, not a pin. Newer Julia
  is fine (CI tests `1.12` + `pre`). The bundle's `.julia-channel` file records `1.12`
  as the **default channel the installer sets up** for users without Julia; an existing
  ≥1.12 install is used as-is. (Context: the fleet dogfoods 1.12; the previously
  declared-but-undogfooded 1.11 floor was a lie the pipeline's load check caught on its
  first run.)
- `deploy/sotd.service` (user-unit template — today it lives only on provisioned hosts and
  must be checked in), launcher scripts, `SHA256SUMS`.

CI grows two jobs alongside the existing Julia workflow:

1. **Rust PR gate** — `cargo build/test/clippy/fmt` (currently nonexistent). Red blocks
   merge.
2. **Tag-triggered release** (`v*`) — build matrix, run tests, assemble the artifacts
   above, generate the changelog with git-cliff from conventional commits, publish the
   GitHub Release.

Cutting a release = run the `/release` skill: bump workspace version, sync Julia versions,
regenerate CHANGELOG, commit `release: vX.Y.Z`, tag, push. CI does the rest.

### 4. Auto-update mechanism

**Each process replaces its own binary; the backend orchestrates.**

- **Check**: `sotd` polls the GitHub Releases API (daily + on-demand), compares against its
  embedded version, announces "vX.Y.Z available" to all FEs over the ADR 0025 daemon→FE
  command channel. The FE shows a badge/toast.
- **Modes** (`settings.toml`): `[update] channel = "stable" | "dev"`,
  `mode = "notify" | "auto" | "off"`. **Default `notify`: no mid-session restart** — the
  update is downloaded and staged in the background, the user sees the badge, and the
  **staged update applies automatically at next launch** (the supervisor picks it up).
  A one-key "apply now" triggers the immediate path. `auto` applies as soon as staged.
  `-dev`-stamped builds never self-update regardless of config (hard guard — the updater
  must not clobber a locally built binary).
- **Apply, FE**: the FE downloads its platform artifact into an `updates/` pending dir,
  then the ADR 0017 path verbatim — sentinel, exit 75, supervisor respawns. Supervisor
  change: stage from `updates/` if a pending binary exists, else from `target/release`
  (dev), else keep the current staged copy. That last branch is what makes the launcher
  work on machines **with no source tree** — the public install is just the staged dir
  plus config.
- **Apply, BE**: download `sotd` + the julia bundle; unpack the bundle to a versioned dir
  (`<data>/sot/julia/vX.Y.Z/`), `Pkg.instantiate` against the shipped Manifest, flip the
  `current` symlink, replace the binary, `systemctl --user restart sotd` (Linux) / restart
  via the launcher's supervision (Windows all-in-one). The kernel-launch path becomes
  "install-root `current` symlink, falling back to repo-relative in dev"
  (`rust/backend/src/kernel.rs`).
- **Ordering**: both sides stage first, then apply together — BE restarts while the FE
  exits 75; the existing reconnect + `last_seen_revision` resume absorbs it. If one side
  lags, the §2 handshake gate turns skew into a clear "update the other side" message.
- **Rollback**: keep the previous binary (`.prev`) and previous julia version-dir. The
  supervisor adds crash-loop detection (non-75 exit within ~10 s, twice) → restore
  `.prev`, mark the version bad, skip it. The BE flips the `current` symlink back.

**Install layout (public, no source tree):**

- Windows: `%LOCALAPPDATA%\sot\{bin,updates,julia\vX.Y.Z + current}`, config
  `%APPDATA%\sot\`.
- Linux/macOS: `~/.local/share/sot/{bin,updates,julia/vX.Y.Z + current}`, config
  `~/.config/sot/`.

### 5. Public install story

- Once release assets exist, public users need **no Rust toolchain** (prebuilt
  binaries). They need Julia; the installer handles it via juliaup.
- `install.sh` one-liners are derived from the sot-setup steps: download the
  selected release assets, lay out the install dir, instantiate the Julia envs
  from the repo checkout, write config, and install the launcher + `sotd.service`.
  Until release assets exist, public installs build from source.
- **Remote-BE-over-SSH is the design, not a dev quirk.** All-in-one uses the identical SSH
  path to `localhost`. Hard requirement, documented and verified by the installer:
  **SSH key auth to the BE host** (for single-machine installs that means a local sshd —
  on Windows the OpenSSH Server optional feature; the installer checks and offers to
  enable it). One code path, no local/remote fork.

### 6. Repo goes public — dev-state relocation

The repo itself will be published. Committed per-machine/dev state was a design flaw;
the fix:

- **Machine/user config moves out of git.** `.sot/hosts.toml`, `.sot/settings.toml`,
  `.sot/keybindings.toml` become gitignored local overrides; committed `*.example`
  templates + docs replace them. The discovery order already supports this
  (`$SOT_*` env → `<repo>/.sot/` → `~/.config/sot/` / `%APPDATA%\sot`) — public users get
  the user-config-dir path; the repo-local `.sot/` stays as the dev override.
  `.sot/worktree.toml` stays committed (genuine project config).
- **Dev-fleet coordination moves to a private sibling repo** (`ship-of-tools-ops`):
  `.claude-bus/` (every `/bus-note` would otherwise be published) and `.claude-memory/`.
  The bus skills get a repo-path indirection; mechanics unchanged.
- **History: scan, then accept — no rewrite.** Nothing secret is in history (hostnames,
  ssh alias names, candid chatter — messy, not sensitive; tokens were never committed).
  Before flipping visibility, run a secrets scanner (gitleaks) over the full history as a
  gate. ADRs stay public; working-session handoff docs live in the private ops sidecar.
- `requirements.md` gets a scope amendment: distribution/public use is currently explicitly
  out of scope there.

### 7. Dev-process changes

- **main stays the integration branch; releases are tags.** The fleet's day-to-day
  (push to main, pull, rebuild) is unchanged; only tagging publishes.
- **Conventional commits become load-bearing** — the public changelog is generated from
  them.
- **Protocol discipline gets teeth**: a non-additive wire change requires a
  `PROTOCOL_VERSION` bump; the handshake gate makes forgetting it visible immediately.
- **Rust CI becomes a PR gate** (new for this repo).
- **Config forward-compat**: `settings.toml`/`hosts.toml` gain a format version and
  boot-time migration once strangers' configs exist; eventually the same for `.concept/`
  sidecars.

## Phasing

- **A — Foundations** (no behavior change; worth it regardless of going public):
  1. `build.rs` version embedding + `--version` for `sot`/`sotd`; `-dev+<sha>` stamping.
  2. `protocol`/`app_version` in HelloReq/HelloRes + BE gate + FE "update needed" screen.
  3. Kernel hello reports real version; BE validates kernel protocol version loud.
  4. `deploy/sotd.service` template checked in.
  5. Rust CI on PRs.
  6. Dev-state relocation part 1: un-commit `.sot/{hosts,settings,keybindings}.toml` →
     `*.example` + gitignore.
- **B — Releases**: `/release` skill; tag-triggered release workflow (matrix per §3);
  julia-bundle assembly with CI-generated Manifests; git-cliff changelog; first tag
  `v0.2.0`.
- **C — Updater**: check/notify in `sotd`; FE badge + apply UX; FE `updates/` staging +
  supervisor pick-up; BE self-update + julia versioned dirs; rollback; channels + `-dev`
  guard.
- **D — Public**: `install.sh`/`install.ps1`; quickstart docs; `.claude-bus`/
  `.claude-memory` → private ops repo; gitleaks history gate; `requirements.md` scope
  amendment; flip repo visibility. **The visibility flip happens ONLY on the maintainer's explicit
  approval, after A–C have been exercised on multiple fleet machines** (per maintainer decision,
  2026-07-01) — no session flips the repo public on its own initiative, ever.

## Consequences

- FE/BE version skew — a real, recurring dev-fleet problem today — becomes a clean,
  self-diagnosing handshake error after Phase A alone.
- The update-apply path reuses the two supervision mechanisms we already trust (ADR 0017
  supervisor, systemd `sotd.service`) instead of introducing a new updater daemon; the new
  surface is download + staging + rollback logic.
- Releases are cheap (tag → CI does everything), so patch releases can be frequent.
- The SSH-key requirement keeps one transport code path but sets an install bar for
  non-technical users (local sshd on Windows all-in-one). Accepted deliberately —
  revisit only if it proves to be the top onboarding failure.
- Public users run frozen Manifests; dev machines keep floating resolution. Dependency
  breakage now has two distinct failure surfaces — CI instantiation of the bundle is the
  gate that keeps releases honest.

## Amendment 2026-07-04 — clone-based install: the repo IS the manual

Panel decision (Codex + Fable converged, maintainer's call). The **julia bundle is
retired**; installs get a **live repo checkout pinned at the release tag**:

- `install.sh` clones `--filter=blob:none` (blobless partial: full history for
  blame, only the tag's tree downloaded) into **`$PREFIX/repo/current`**, then
  instantiates `julia/kernel` + `julia/repl` inside it. Prebuilt binaries stay
  the run path, installed alongside as before.
- `resource_dir` resolves `<exe>/../repo/current/<rel>` first (legacy
  `julia/current` bundle layout kept as fallback for pre-clone installs).
- **One update path**: re-run the installer with the new version — a dirty
  checkout **refuses to move** (read-only by convention, fail loud), update =
  `fetch --tags` + `checkout <tag>` + binary swap, and **HEAD must equal the
  tag's recorded commit** on every install/update (a moved tag or half-checkout
  dies at install time, not first use).
- Release CI: the julia-bundle job is retired; its real value — "the julia envs
  resolve and load at this exact ref" — survives as the release-blocking
  `julia-check` job (this check caught the Julia-1.11 `@K_str` break).
- **The repo is the manual**: the FE Terminal's agent reads the checkout as the
  product's help system. The daemon exports `SOT_MANUAL=<checkout root>` into
  every workspace tmux session (resolved via `resource_dir`, so dev trees work
  identically); `docs/USING.md` (docs-lane deliverable) is the entry point for
  the user-facing help+extend persona.

Rationale (over a curated docs corpus): a tag checkout is complete by
construction — every bundle bug (missing `examples/`, missing sidecars, stale
Manifests) was a curation gap, a class this deletes; the agent-as-help-system
needs docs + ADRs + source together (docs answer "how", the rest answer "why"
and "make it do X"); and docs/binary skew becomes structurally impossible since
both move on one tag. Risk accepted: checkout weight (media grows per release)
— mitigated by the blobless clone and, if ever needed, moving heavyweight
media to release assets.

## Public baseline hygiene

For the sanitized public baseline, operational content lives in the private
`ship-of-tools-ops` sidecar and public install docs must not assume release
artifacts exist. Verify current tags and GitHub Releases before using the
release-installer path; if no matching assets exist, install from source.
Installer GitHub auth remains optional and is only a rate-limit dodge for public
API calls.
