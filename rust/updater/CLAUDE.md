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
- `src/pending.rs`: the `pending-<target>.json` pointer that arms one release for the next launch.
- `src/platform.rs`: which target triples ship, and the asset filename for a version and triple.
- `src/prepare.rs`: transactional preparation of a version (checkout and Julia instantiate) and the children it runs.
- `src/select.rs`: channel selection, the tag a given installed version should track.
- `src/semver.rs`: semver parsing and ordering for release tags.
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
