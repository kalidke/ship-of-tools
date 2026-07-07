# API — Kernel

The Julia **kernel** (`ShipToolsKernel`) is the plugin host process. It owns the
dispatch tables, computes mode trees, indexes the project, hashes ASTs, and
produces Julia-aware previews — exposing all of it to the backend daemon over an
NDJSON line protocol.

!!! note "Reference under construction"
    Per-symbol API docs for `ShipToolsKernel` are not yet wired into this site (the
    kernel module is not currently a dependency of the docs build). For now:

    - The wire protocol it speaks: [Line Protocol](../design/protocol.md).
    - The ABI it dispatches on: [API — ConceptExplorerCore](api-core.md) and
      [The Dispatch ABI](../extend/abi.md).

    Source: `julia/kernel/src/ShipToolsKernel.jl`.
