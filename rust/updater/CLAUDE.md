# rust/updater: release discovery and staging primitives (distribution)

Pure mechanism, no policy: find the release a box should run, fetch and verify its bytes, and stage them under
`<updates-root>/<tag>/`. A stage is complete if and only if its ready manifest parses and matches the wanted identity,
never because a marker file exists. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `Cargo.toml`: the sot-updater package manifest.
- `examples/`: `vercmp`, a command-line check that a candidate tag sorts above the existing tags of its line.
- `src/fetch/`: the release bytes: download backends, SHA256SUMS, sha256 and archive extraction.
- `src/identity.rs`: `ReleaseIdentity`, the one release for one platform that every stage must agree on, and the release repo a box watches (`DEFAULT_REPO`, `repo_from_env`).
- `src/lib.rs`: discovery (`check_release`) and staging (`stage`), the crate's public surface.
- `src/lock.rs`: the cross-process mkdir lock on `updates/.lock`, shared with sot-apply.
- `src/manifest.rs`: `InstallManifest` (`$PREFIX/install.json`, a schema shared with scripts/install.sh and
  scripts/install-manifest.ps1) and the ready manifest of a stage.
- `src/pending.rs`: the `pending-<target>.json` pointer that arms one release for the next launch: `arm` (the only writer), the install it was armed over (`InstalledBuild`, read from `<prefix>/install.json`) and `stale_reason`, the apply-side judgement.
- `src/platform.rs`: which target triples ship, and the asset filename for a version and triple.
- `src/prepare.rs`: transactional preparation of a version (checkout and Julia instantiate) and the children it runs; a
  release checks out its tag, a dev build (`semver::is_dev`) the COMMIT its stage recorded, since it has no tag.
- `src/select.rs`: channel selection, the tag a given installed version should track.
- `src/semver.rs`: semver parsing and ordering for release tags and the line's `-dev` stamps.
- `src/spawn.rs`: the required caller-owned `Spawner` interface; no production native spawner is supplied by this crate.
- `src/unique.rs`: unique names for temp dirs and lock nonces.

## Start here
`check_release` (discovery) and `stage` (staging) in `src/lib.rs`; read them first for any change to what the updater finds or
leaves on disk.

## Rules
- `stage` verifies the checksum before extraction and renames the stage into place last; an incomplete stage is never
  taken for a finished one.
- Staging runs under the `updates/.lock` taken in `lock.rs`; sot-apply uses the same lock.
- Every spawn-bearing entry, including `check_release`, `stage`, `prepare::prepare` and `prepare::PreparedState::matches`, requires its caller's `Spawner`. Fetch, archive and prepare helpers use that spawner for all process output; production updater code starts no process directly. The daemon supplies containment and the window supplies its own lifetime policy. The stage lock's liveness probe starts no process (`pid_alive`).
- `pending::arm` reads `<prefix>/install.json` fresh under the staging lock. An automatic `Writer` arms only a version strictly above it (`ArmOutcome::NotAboveInstall` otherwise) and never into a dev build, a `+` in the installed version (`DevInstall`), nor into a pinned install, `install.json`'s `pinned` set by a run that named its version (`Pinned`; `InstalledBuild::pin_note` is the verdict's words, and the check's status and the window's self-update say it too); the operator's `Writer::Operator` (`sotd update stage`) may arm the same or an older build and replaces any armed pointer. An `install.json` that exists and cannot be read may hold a pin (`InstalledBuild::try_read` is the three-state read: `Ok(None)` absent or with no version and no pin, `Err(UnreadableInstall)` cut, not JSON, or pinned with no version, `Ok(Some)` read; a leading BOM reads), so an automatic arm returns `UnreadableInstall` and the daemon's check, the pipeline, the window's guard refuse over it (`sotd update pointer` and `update.apply` refuse a pointer armed over a record), all in `UnreadableInstall::sentence` (the file, the cause, the installer run that repairs it on this OS); the operator's arm (`Writer::Operator`, `sotd update stage`) proceeds as over no record, which is how `install.ps1 -Version` repairs it (and a run that names no version, since `sotd update direction` reads the version text an unparsable record still holds, or says `unknown`, which that run takes as "go on"). On Windows only, an `install.json` that is missing while `bin\sotd.exe` exists (`missing_record`) is refused the same way, since Windows writers replace the record in a step that can leave it briefly missing and install.ps1 writes it before any daemon starts; the Unix writers rename atomically and a fresh install.sh writes it last, so there absent is no install. `InstallManifest::of_current_exe` is the exe-located three-state read, and a file there decides the updates root parsed or not. `InstalledBuild::version_or` gives callers the install's version, read now, for the channel and the comparison. Every pointer records the install it was armed over (`armed_over_version`, `armed_over_commit`), and `stale_reason` says why a pointer no longer fits the install on disk (its record no longer matches it, or, with no record, it is not above the install or the install is a dev build). The scripts read none of this: they ask `sotd update pointer`.
- A change to the pointer's format is additive and optional: a new field has a serde default and an unknown field is ignored, so every pointer any build ever armed stays readable. The apply side refuses a pointer it cannot read (`sotd update pointer`), and a field that became required would turn every pointer already armed into a refusal.
