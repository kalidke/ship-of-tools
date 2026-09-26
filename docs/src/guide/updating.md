# [Updating and rollback](@id updating)

Updating a release install is one command. On a `--local` install and on
Windows, an update whose frontend crash-loops right after applying is rolled
back automatically.

Two mechanisms complement rather than compete (release installs only —
**source builds are stamped `-dev` and never self-update**; they update by
`git pull` + rebuild):

| Mechanism | What it does | What you do |
|-----------|--------------|-------------|
| **Built-in update check** | The backend checks GitHub for new releases (daily and on demand, plain HTTPS, no GitHub auth), **notifies the frontend**, and prepares the whole new version in the background: binaries, a tag-pinned checkout and instantiated Julia environments, verified against the release's `SHA256SUMS`. The staged version is applied at the next launch (or daemon restart) as an offline switch, with the previous version kept for rollback. | Nothing — you'll see the notice; relaunch when convenient. |
| **Re-running the installer** | The **complete** update in one command: new binaries *and* the repo checkout moved to the new tag *and* Julia envs re-instantiated, atomically. It is also what migrates a pre-0.6 install onto the auto-update layout. | `curl -fsSL …/install.sh \| bash -s -- <your-role>` (same command as install; add `--version vX.Y.Z` to pin). |

**When in doubt: re-run the installer.** It is idempotent and moves
everything together. With an agent, say:

```text
Update Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md and follow it. If this machine runs a source build, replace it with a release install.
```

`SOT_UPDATE_MODE` controls the built-in check: `notify` (the default) stages
and applies at the next launch, `off` disables it, and `auto` additionally
restarts the backend to apply once no frontend is attached. That restarts
REPLs and kernels, so their in-memory state is lost, which is why it is
opt-in; agent sessions survive it.

On **Windows** the frontend runs the same check itself, since there is no
local backend to do it, and `launch-sot.ps1` applies the staged update at the
next launch through `scripts\sot-apply.ps1`: the same verification and
rollback, and a new build that crashes within 10 seconds of its first launch
is reverted automatically.

On update the installer swaps binaries while keeping `.prev` copies, fetches
tags in `$PREFIX/repo/current`, checks out the requested tag, **refuses to
move a dirty checkout** (commit/stash/revert first — the checkout is
read-only by convention), and verifies `HEAD` equals the tag's recorded
commit, so a moved tag or half-checkout fails at update time, not at first
use.

## Pinning and going back

- **Pin a release.** Add `--version vX.Y.Z` to the install command to move to
  (or stay on) a specific tag; the same flag moves you *back* to an older tag.
- **Automatic rollback.** Applying a staged update is an offline pointer flip
  done by `sot-apply`, which records the previous install as *last good*.
  The `--local` launcher runs `sot-apply --rollback` if the frontend exits with an error
  within 10 seconds twice in the 30 minutes after an update; Windows does the
  same through `launch-sot.ps1`. A `--backend` (frontend-only) install on
  Linux or macOS does not roll back on its own: re-run the installer with
  `--version` to go back.
- **Keep the checkout clean.** The installed checkout under
  `$PREFIX/repo/current` is the manual and the resource tree. Updates refuse to
  move a dirty tree, so leave it read-only.
