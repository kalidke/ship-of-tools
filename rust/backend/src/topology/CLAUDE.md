Part of topology: the contract is `rust/topology/CLAUDE.md`. This folder is the daemon's half, which stays in the `sotd`
binary (it needs the roster, rows and the lease record): `cli.rs` (`sotd topology`), `relay_units.rs` (apply, refresh,
pin), `store.rs` and `set.rs` (`TopologyStore`, op `topology.set`), `status.rs` (`sotd status`), `same_version.rs`,
`stdio_bridge.rs`, `dial.rs` (the one-shot blocking client).
