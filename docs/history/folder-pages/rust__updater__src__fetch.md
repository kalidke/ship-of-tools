# rust/updater/src/fetch: getting and proving a release's bytes (distribution)

The files that download a release and prove it before it is used: the backends, the checksum list, the hash and the
archive extraction. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `mod.rs`: the `Fetcher` backends (curl, gh and dir) and the release check's scratch folder.
- `archive.rs`: allowlist-validated listing and extraction of a release archive.
- `sums.rs`: SHA256SUMS: parse, lookup and release discovery.
- `hash.rs`: streamed sha256 of a file.

## Start here
`Fetcher` in `mod.rs` for a new download source; `extract_validated` in `archive.rs` for what an archive may hold.

## Rules
- Checksums are verified before extraction (`stage` in `src/lib.rs` hashes with `hash::sha256_file` against the
  `sums::lookup` entry first).
- Every archive entry is validated against the expected top folder before anything is written
  (`archive::extract_validated`).
- A release check's scratch folder is a held folder (`sot_log::host::HeldDir`) in the system's temporary folder: the
  check removes its own, and every check first removes those of checks whose process has ended (`remove_ended`), never
  one whose process still runs (`scratch_dir`, `remove_ended_scratch`). A temporary folder on a file system that refuses locks
  (NFSv3 without its lock service) fails every check with "making the release check's scratch folder".
- Discovery, downloads, archive listing and extraction run their commands through the required caller `Spawner`, preserving their existing timeouts and error contexts; this folder owns bytes and validation, not child lifetime policy.
