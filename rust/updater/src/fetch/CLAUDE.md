# rust/updater/src/fetch: getting and proving a release's bytes (distribution)

The files that download a release and prove it before it is used: the backends, the checksum list, the hash and the
archive extraction. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `mod.rs`: the `Fetcher` backends (curl, gh and dir) and the scratch dir.
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
