---
target: files/rust
target_kind: directory
synced_against: spike-placeholder
synced_at: 2026-05-11T15:13Z
authored_by: orchestrator
---

# Rust workspace

Houses the three Rust binaries: `frontend` (winit + wgpu + ratatui chrome,
native local window per ADR 0003), `backend` (tokio + interprocess
daemon, owns project state), and shared `protocol` crate (NDJSON
envelope types per ADR 0001).

Built per workspace at `rust/Cargo.toml`. Build all: `cargo build --release`.
