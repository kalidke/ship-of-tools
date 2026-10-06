# rust/log/tests/fault_storage: storage exhaustion tests (capsule)

Private bounded-volume fixtures test the real capsule IO boundaries on Linux, macOS and Windows.
Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `main.rs`: the suite entry, bounded fixture-child dispatch and owned resource guards.
- `volume.rs`: native private-volume setup, real allocated writes, ballast removal and cleanup.
- `boundaries.rs`: executed native-error caller checks at preflight, write, reset, transport bind and producer startup.
- `surface.rs`: prepares disposable parent, prospective and reversal source copies with passive IO overlays, records snapshot identities and checks executed premise test results. Selected snapshot bodies use the existing isolation entry check.

## Rules
- A missing required native fixture fails loudly; no ignored or silently skipped premise.
- Fill only the owned bounded volume; logs, binaries and control resources live outside it.
- Cleanup addresses only resources created and retained by this fixture.
- This temporary CI branch prepares P1/P2 only. Prospective repairs and passive IO injections exist only in disposable source copies; the lane remains at its released base.

- Hosted Ubuntu's explicitly selected ext4 loop image is the sole Linux full-volume evidence route. macOS uses hdiutil's `-format UDRW`; Windows retains validated drive-qualified native-tool paths for setup and teardown. Setup, entry and cleanup failures are harness failures, never storage assertion reds.
