# rust/log/src/supervisor/probe: is a leg there, and is it ours (capsule)

The supervisor asks one question before it spawns or adopts a leg: is something already there, and does it belong to this
user. This folder answers it: a platform-neutral seam (`ProbeOps`), one classifier that turns its observations into a
`ProbeOutcome`, and one real implementation per OS. Part of the capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the `ProbeOps` trait, the mechanical outcome enums (`ConnectOutcome`, `SpawnOutcome`, `WaitOutcome`, `FenceProbe`) and `ScriptedProbeOps`, the scripted test double
- `classify.rs`: the classifier: `probe_owned_spawn` (spawn, then Stage A and B) and `probe_adopt_only` (Stage B alone), with `ProbeOutcome`
- `leg_process.rs`: `LegProcess`, what the authority keeps of a live leg: the child it spawned (`Owned`) or a proven identity (`Adopted`)
- `unix.rs`: Linux `RealProbeOps` and `SpawnedChild`, over pidfds
- `win.rs`: Windows `RealProbeOps` and `SpawnedChild`, over a named pipe and a spawned process
- `macos.rs`: macOS `RealProbeOps` and `SpawnedChild`, over a kqueue `NOTE_EXIT` watch

## Start here
`probe_owned_spawn` and `probe_adopt_only` in `classify.rs` for what an observation means; `ProbeOps` in `mod.rs` for what
an OS implementation must provide.

## Rules
- The classifier makes no OS call of its own: everything goes through `ProbeOps` (`classify.rs`), so its tests run on every platform with `ScriptedProbeOps`.
- Stage A resolves an owned child completely, challenge included, before the episode deadline is consulted (`probe_owned_spawn`).
- The episode deadline and the attempt cadence come from the caller, never from this folder; only the per-attempt challenge deadline is derived here, clamped to the caller's boundary (`clamped_challenge_deadline`).
- Each OS file is self-gated by its own `#![cfg]`.
- A leg this supervisor spawned is kept as its child from spawn to its one reap: `Ready` hands the child back through `retain_owned_child`, and `LegEnded` carries its exit status. The status (code or signal) is read once, at the Unix reap or after the Windows exit wait, and kept; an adopted leg's status is `None`.
