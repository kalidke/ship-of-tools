# ADR 0030: Versioning, releases, and auto-update — going public

**Status:** Accepted (design approved 2026-07-01: publish this repo; Windows+Linux
x86_64 at launch with macOS experimental; update default = notify in-session + auto-apply at
next launch; remote-BE-over-SSH is the design — all-in-one uses the same SSH path to
localhost, SSH key auth to the BE host is a hard requirement)
**Date:** 2026-07-01

> **Superseded in part (2026-08-10 note):** two claims above and in the body
> drifted from the shipped implementation — read the **Amendment 2026-07-04**
> below first. (1) The **julia bundle is retired**: every `julia-bundle-*`
> asset / unpack-and-symlink passage below describes the pre-amendment
> design; installs now clone the repo at the release tag. (2) The implemented
> `--local` role connects over the **local Unix socket directly** — SSH key
> auth is a hard requirement only for remote-backend layouts, not
> all-in-one.

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
- **The FE also surfaces the skew continuously, not only when it is fatal.** The
  handshake gate above only fires on *protocol integer* mismatch; two builds can differ
  in product version while speaking the same wire contract — the common dev-fleet case
  (rebuild one side, forget the other). So the FE paints a `fe <ver> · be <ver>` stamp
  on the bottom chrome edge, sourced from `app_version()` and `HelloRes::app_version`,
  dark gray when the halves agree and **yellow when they don't**. Both halves are always
  shown: collapsing to a single version when they match would hide exactly the field
  being watched. An unknown BE (pre-hello, or a pre-versioning daemon) renders `be ?` and
  is not treated as skew.
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
- *(retired — see Amendment 2026-07-04)* `julia-bundle-vX.Y.Z.tar.gz` — the `julia/` tree (kernel, repl, plugins, pluto) + the
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

  **Amendment (Windows applier).** The "pending binary" above was originally a literal
  `updates\pending\sot.exe` that `launch-sot.ps1` moved into place. Phase C3 replaced
  that contract with a per-target *pointer* (`updates/pending-<target>.json`) naming a
  verified stage under `<tag>-<target>/`, consumed by `sot-apply`. The PowerShell
  launcher was never migrated, so on Windows it kept testing a path nothing writes —
  dead code — and there was no `sot-apply` for the platform (`sot-apply.sh` exits early
  on any `uname` outside Linux/Darwin). Windows therefore had **no working apply step at
  all**, compounded by `install.sh` — the sole writer of `install.json` — refusing to run
  there, which meant the FE's `spawn_startup_selfcheck` never got past its
  "not a release install" guard and never even checked.
  Closed by `scripts/sot-apply.ps1` (the Windows applier: same verify → swap-with-`.prev`
  → flip → rewrite-manifest → arm-marker transaction, using directory **junctions**
  instead of symlinks so no Developer Mode or elevation is needed) and
  `scripts/install-manifest.ps1` (writes `install.json`, called from
  `install-shortcut.ps1`). `launch-sot.ps1` now invokes the applier before launch and
  delegates crash-loop rollback to `sot-apply.ps1 -Rollback`, so a revert restores the
  whole transaction and marks the bad tag, rather than only copying back the `.exe`.
  Regression cover: `scripts/tests/test-sot-apply.ps1`, run on the CI Windows leg.
- **Apply, BE** *(bundle mechanics retired — see Amendment 2026-07-04; the binary swap and restart survive)*: download `sotd` + the julia bundle; unpack the bundle to a versioned dir
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

## Amendment 2026-08-13 — Phase C shipped: transactional versioned auto-update

Phase C is implemented (PR series C1–C4), with the design **revised by an
adversarial Codex review** before implementation: after the clone-based
install amendment, a version is *binaries + tag-pinned checkout + Julia
envs*, so §4's original "each process replaces its own binary" model was
replaced by **whole-install transactions**. What shipped:

- **Discovery is gh-free** (`sot-updater` crate, shared by `sotd` and `sot`):
  `GET releases/latest/download/SHA256SUMS` over plain HTTPS via `curl` — one
  documented request yields the released version (derived from the
  deterministic asset names) and the digests. Every later fetch is pinned to
  the derived tag; a full release identity `{repo, tag, version, target,
  asset, sha256}` travels through every stage, and tag/repo strings are
  strictly validated before touching a URL or path. `SOT_UPDATE_FETCHER=gh`
  remains for private forks; `dir:<path>` sideloads (and drives the tests).
- **Prepare at stage time, OFF the live tree**: the install layout is now
  `repo/base` (blobless fetch target) + `repo/versions/<tag>` (detached
  worktrees) + `repo/current` (symlink). The daemon stages (download →
  streamed sha256 → allowlist-validated extraction → ready manifest) and
  prepares (worktree at the tag's commit, HEAD==commit gate, Julia
  instantiate + load-test, mathjax `npm ci`) in the background, then **arms**
  an atomic pending pointer (newer-wins; crash-loop-marked versions refuse to
  re-arm). `git checkout` never runs over `repo/current` — the live daemon
  resolves resources through it.
- **Apply is a fast offline flip with ONE owner per platform**:
  `sot-apply` (shipped in `<prefix>/bin` AND inside every release archive)
  re-verifies digest + commit, swaps binaries keeping `.prev`, flips the
  `current` symlinks, rewrites `install.json`, clears the pointer. Owners:
  systemd `ExecStartPre=-` (Linux service), `sot-launch` (macOS /
  `--no-service`, including `.app` launches — it also supervises the FE now:
  exit-75 respawn on Unix, crash-loop → `sot-apply --rollback` → previous
  version restored + `bad-<tag>` marker). The `update.apply` op arms, acks,
  and exits — "arms" meaning it validates that the pipeline already armed a
  pointer (it does not arm on its own); it never applies in-process.
  Cross-process serialization is a mkdir lock under the updates root
  (owner-nonce verified: breaks and releases only ever touch a lock whose
  recorded owner matches the observation).
- **Remote FEs self-stage**: the frontend runs its own check→stage→prepare
  (checkout-only)→arm at startup on `remote`-role installs, independent of
  the control channel (a protocol mismatch kills the connection before any
  op — the exact case an updater must survive). Non-remote roles defer to
  the backend pipeline so an env-less prepare can never arm on a BE host.
- **`install.json`** (schema 1, written by the installer, updated by apply)
  records role/prefix/config/service/version; binaries resolve their staging
  root from it (fixes `--prefix` installs). No temp-dir staging fallback —
  unresolvable roots fail loud.
- **Modes**: `notify` (default — stage+prepare+arm, apply at next launch),
  `auto` (additionally exits for the apply owner when **zero clients** are
  attached — caveat: detached tmux workspaces/REPLs don't count as attached
  and can be interrupted; that is why auto is opt-in), `off`. Config surface
  is env (`SOT_UPDATE_MODE/FETCHER/REPO/ROOT`); a `settings.toml [update]`
  table is deferred until the FE/BE settings parsers are unified.
- **Release pipeline hardening** (it is the auto-updater's code-execution
  trust root): workflow token is read-only with `contents: write` scoped to
  the publish job, all actions pinned to commit SHAs, and all three platform
  artifacts are smoke-tested with exact-version asserts (Windows previously
  shipped unverified).

A second adversarial Codex pass over the full diff added (all shipped):
**commit binding** — releases publish a sums-covered `COMMIT` file and
prepare refuses a tag whose commit disagrees with what the binaries were
built from (moved-tag defense); **per-target transaction state** — pending
pointer, bad markers, last-good, and the health marker are all
`-<target>`-suffixed and the applier enforces its own host target (shared
`$HOME` roots serve several platforms); **per-file digests** — stages write
`files.sha256` and apply verifies the ACTUAL binaries it installs, dropping
a damaged stage + pointer so the pipeline re-stages instead of looping;
**apply is all-or-restore** — any post-mutation failure restores previous
binaries and symlinks before exiting, and success arms a 30-minute
crash-loop health window (rollback fires only inside it, never on unrelated
crashes weeks later; a manual installer run clears stale rollback state);
**single apply owner enforced in the launcher** — systemd installs route
through `try-restart`/`ExecStartPre` with the daemon stopped, launcher-owned
daemons are stopped before applying; **safe migration** — the old in-place
clone is probed loudly and preserved at `repo/current.pre-versioned`, never
deleted on a failed diagnostic.

**Deliberately deferred, tracked in the ops sidecar**: artifact signing
(integrity = GitHub TLS + SHA256SUMS + strict validation + pipeline least
privilege, documented trade-off); Windows production auto-update (needs the
Phase-D `install.ps1` + supervisor rework — the shared crate already handles
Windows staging); macOS launchd wiring and daemon-side crash-loop
auto-rollback (manual: `sot-apply --rollback`, or re-run the installer);
refreshing agent comm resources (`update_comm`) on auto-apply; routing the
INSTALLER's own upgrade path through prepare/apply (it remains a live update
with the safety probes above); flush-coupled `update.apply` exit; refreshing
installed control-plane artifacts (`sotd.service`, the `sot-launch`
heredocs) from the archive at apply time; persisting repo/fetcher choice in
`install.json` for private-fork installs.

**Migration**: existing release installs pick up the versioned layout on
their next installer re-run; installs without an `install.json` get
check/notify/stage against the legacy path but never prepare/arm (fail-safe:
they keep working exactly as before, updated manually).

## Amendment 2026-09-08 — §8: every runtime answers what build it is

Prompted by a field incident: a launcher pair rebuild left a pinned
`sot-capsule` binary behind, and every capsule it had already spawned became
permanently unreachable with no operator-visible signal beyond a bare
`workspace.list` row that merely *looked* started. §1 above owns "one product
version"; §2 owns the protocol gate. Neither owns the fact that a capsule
runtime has a THIRD identity — the supervisor lane's own build-boundary id
(ADR 0041/0043) — and nothing on the wire exposed it. This amendment closes
that gap: it defines what a version string honestly means and adds the one
op that lets any caller ask any runtime what it is. Cross-referenced from ADR
0043's own decision list as **decision 31**.

**The identities, and who enforces them today:**

| pair | matched on | enforced at | visibility before this amendment |
|---|---|---|---|
| frontend ↔ daemon | `PROTOCOL_VERSION` (hard) | the hello gate (§2) | a rejection screen |
| frontend ↔ daemon | `app_version()` (soft) | nothing | the FE's own fe/be strip stamp |
| daemon ↔ its `sot-capsule` | `SUPERVISOR_LANE_BUILD_ID` | the pair-verdict check before every spawn | a spawn error only |
| daemon ↔ a running supervisor | `SUPERVISOR_LANE_BUILD_ID` | the supervisor's own hello | erased to a bare "unreachable" |
| supervisor ↔ its leg | nothing, by design | — | n/a (ADR 0041 Lifecycle: adopting a surviving leg across a supervisor restart is the point) |

The fourth row was the field incident. `workspace.list`'s capsule-phase probe
already detected a foreign-build refusal internally (it text-matches the
error to log a one-time operator warning) and then discarded that fact one
line before the wire, folding it into the same `"unreachable"` a merely-dead
lane reports. Not a missing probe — a discarded fact.

**Decisions:**

**(a) `app_version()` gains a `-dirty` suffix.** Two builds a real build-
boundary check would refuse to pair could, before this, print the identical
version string — the protocol crate's `build.rs` emitted a short sha with no
dirty flag, unlike the capsule-runtime build id, which already carried one.
Format becomes `X.Y.Z-dev+<short sha>-dirty` whenever the working tree had
uncommitted changes at build time (fails closed: an unverifiable tree counts
as dirty, never assumed clean); a tree that is ON its release tag but ALSO
dirty is not the release it claims to sit on, so it takes the `-dirty` form
too rather than collapsing to the bare tag version. Invariant: a version
string never claims to be a commit it was merely built FROM. Two distinct
dirty trees at one HEAD still alias on this scheme — the same hole the
capsule-runtime build id already documents as deferred (a release never
ships a dirty build, so it never reaches production); this amendment does
not invent a second answer to it.

**(b) A new op, `version.query`.** Pure in-memory, no fan-out, no supervisor
probe: `{}` in, `{ daemon: { app_version, protocol, lane_build }, clients: [
{ client_id, app_version, protocol } ] }` out. `lane_build` is
the supervisor-lane build id this daemon demands of any capsule it attaches,
adopts, or spawns — the fact every pair-verdict check already has and had
nowhere to report. `clients` is every attached frontend, sourced from the
hello each already sent (an operator on the backend can now name the build
each frontend is running without seeing its screen) — this is why the op
cannot simply be folded into `hello`: hello answers once, self-only, before
later clients ever connect. Capsule rows are deliberately NOT in this reply:
`workspace.list` already fans out to every supervisor per call and already
carries `phase`, so a second fan-out here would only duplicate it with its
own timeouts. Legacy compatibility here is about who ANSWERS, not the
schema: this is a WHOLLY NEW op, so its response's own fields need not be
`#[serde(default)]` to avoid breaking an old peer — an old daemon never
constructs one at all, answering instead with the ordinary generic
unknown-op payload on a `res` frame carrying the same op, which every
caller of this op must treat as "daemon predates this op," never a
failure. `clients` IS `#[serde(default)]` regardless, for the ordinary
reason any collection field is: a future daemon that answers this op but
omits the roster should still deserialize to no clients, not fail.

**(c) One new value of an existing field.** `WorkspaceListEntry.phase` gains
`"foreign"`: the lane answered and refused this daemon's build
(`version_skew`), as opposed to `"unreachable"` (no answer at all). This is
typed, not text: the supervisor-lane exchange itself now flags whether the
terminal refusal was SPECIFICALLY a `Refused { VersionSkew }` reply, surfaced
as a dedicated `sot_log::Error::VersionSkew` the daemon's own capsule-phase
probe matches on directly — a malformed reply, trailing bytes, or a wrong
pid/creation is `Foreign` too at the exchange level, but NONE of those are
version skew, and only the typed check can tell them apart (a text match
against a broader "foreign" classification cannot). No new field, no new
wire shape: the frontend's existing phase-tag rendering picks it up for
free. The foreign supervisor's own build id is deliberately NOT put on the
wire — `[foreign]` plus this daemon's own `lane_build` (from `version.query`)
already says everything an operator would act on differently, and the
foreign build id would require widening the supervisor lane's own refusal
frame for a string nobody would act on differently.

**(d) `sotd --help` answers.** It used to fall through to the argument
parser's generic "unrecognised argument" bail — folded into the same early,
side-effect-free query arm as `--version`.

**Where this is shown:** the frontend paints `[foreign]` in the same yellow
its fe/be skew stamp already uses, and — the "blank page" fix — a capsule
attach client that dies terminal before ever checkpointing now paints one
line naming why in the pane itself (previously the reason existed only in
the status bar while the pane fell through to an unrelated, usually-blank
tmux screen), read from the retained client's OWN status directly so a
later, unrelated status-bar write can never retitle it; that terminal
reason is also mirrored to `tracing` now, not only the pane's own status
string. A `sot-fe version` shell verb prints one table — daemon identity
(labeled by its resolved endpoint, never a "local"/"backend" guess),
attached clients, and one row per capsule workspace with its phase
PRINTED VERBATIM: no derived verdict column — this script never
challenges the lane itself, so it has no proof to claim beyond the
phase the daemon already reported (`"foreign"` already IS the verdict) —
and a row for the locally installed comm scripts. That last row is new
too: `install_comm` now stamps `$SOT_COMM_HOME/VERSION` (dirty-suffixed
when the source checkout has local edits), written only after every copy
has actually succeeded, with the repo commit at install time — the fact a
send-deaf Windows bridge incident had no way to state ("the scripts on
this box came from commit X").

**Deliberately left out** (same reasoning as the rest of this ADR's
"smallest useful slice" posture): a capsule row's own build id on the wire
(covered by (c)'s own reasoning); `lane_build` duplicated onto `HelloRes`
(one op should answer one question once); a background version-skew sweep
(reactive over eager — the frontend already asks at hello and on every
`workspace.list` poll); hashing a dirty tree's content for a unique dirty id
(real, bounded work already deferred alongside the capsule-runtime build
id's own identical gap); a `clients.list` presence op (the two new
`ClientInfo` fields ride the existing, previously-unused `peer`/
`connected_at` capture — no new op for it); auto-remediation (a daemon
killing a foreign supervisor, or rebuilding itself) — that is the pinned U4
upgrade transaction; this amendment makes skew *visible*, and must not also
make it silently disappear.

## Public baseline hygiene

For the sanitized public baseline, operational content lives in the private
`ship-of-tools-ops` sidecar and public install docs must not assume release
artifacts exist. Verify current tags and GitHub Releases before using the
release-installer path; if no matching assets exist, install from source.
Installer GitHub auth remains optional and is only a rate-limit dodge for public
API calls.
