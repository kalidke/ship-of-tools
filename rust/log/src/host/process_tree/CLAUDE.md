# rust/log/src/host/process_tree: the native process-birth primitives (platform)

The operating-system half of "a process starts inside its owner's reach". On Unix a fork whose child waits at a gate
is returned to its caller before the target runs: the caller owns the child (its parent, so the only one that can
wait for it), sees its report that it is set up, and says GO or CANCEL. The native branch of the child runs no Rust.
Part of platform; charter: rust/log/src/host/CLAUDE.md.

## Files
- `mod.rs`: the module doc; re-exports the Unix launcher
- `unix_birth.rs`: `Launch` (everything fixed in the parent), `Birth` (the owned, provisional child) and the FFI to the native launcher
- `native_birth.c`: the launcher: the fork, the child's one native branch, the gate and the status records
- `native_birth.h`: the fixed C ABI of the launcher

## Start here
`native_birth.c`, `child_branch`: what runs in the child before the target's exec, and nothing else.

## Rules
- The child branch calls only async-signal-safe operations over a description built before the fork: no allocation,
  no environment access, no logging, no Rust, no return (`child_branch`).
- `Launch::begin` returns at once with the owned child; the target has not run, and a closed gate, a CANCEL, a malformed
  byte or the owner's drop ends the child before it does (`Birth::release`, `cancel`, `Drop`).
- Every endpoint the child must not hold is closed first (`close_in_child`); a descriptor that must cross the exec is
  named (`inherit_across_exec`), and only its copy in the child loses its close-on-exec flag.
- Dispositions are reset to default and the mask to empty before the exec, the two glibc-reserved signals through the
  kernel call (`reset_disposition`).
- The launcher is compiled by `rust/log/build.rs` for the Unix targets, from a macOS host only for a macOS target; the
  `native-barrier` build adds the test pauses and no installed build has them.
