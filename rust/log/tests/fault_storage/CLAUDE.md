# rust/log/tests/fault_storage: storage exhaustion tests (capsule)

Private bounded-volume fixtures test the real capsule IO boundaries on Linux, macOS and Windows.
Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `main.rs`: the suite entry, bounded fixture-child dispatch and owned resource guards.
- `volume.rs`: native private-volume setup, allocation, ballast removal and cleanup.
- `boundaries.rs`: preservation of native storage errors at preflight, write and reset boundaries.
- `surface.rs`: parent/prospective/reversal source copies and anchored passive IO overlays for the native premises.

## Rules
- A missing required native fixture fails loudly; no ignored or silently skipped premise.
- Fill only the owned bounded volume; logs, binaries and control resources live outside it.
- Cleanup addresses only resources created and retained by this fixture.
- This temporary CI branch prepares P1/P2 only. Prospective repairs and passive IO injections exist only in disposable source copies; the lane remains at its released base.
