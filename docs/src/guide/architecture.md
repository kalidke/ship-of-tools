# Architecture at a Glance

Ship of Tools is **three processes plus your REPL**, talking over a socket — even when
everything runs on one machine. This page covers what each process owns, why the
split falls where it does, the one serialization seam between Rust and Julia, and
how the same architecture runs a remote project with almost no extra plumbing.

## The processes

| Process | Language | Owns |
|---------|----------|------|
| Frontend | Rust (winit + wgpu) | the native window, chrome, preview rendering, keystrokes. Stateless about the project — it renders what the backend sends and forwards input. |
| Backend daemon | Rust (tokio) | project state, file watching, process supervision, the orchestrator LLM session. Exposes a JSON line protocol to the frontend. |
| Julia kernel | Julia | dispatch tables, mode-tree computation, file-type-aware indexing, AST hashing, Julia-aware previews. Loads the project's `Project.toml` environment. |
| REPL | Julia | your interactive session, supervised by the backend, with a display shim that emits structured frames (stdout, stderr, value, image, error) over stdio. |

The frontend holds no project knowledge. The backend is the single owner of
project state and the one protocol entry point. The Julia kernel is where every
extensible, Julia-aware decision is made. The REPL is a separate process so a
crash or a long computation in your interactive session never takes down the
daemon that supervises it.

## Why client/server, even on one machine

The three processes communicate over a socket locally exactly as they would
across a network. That looks like overhead on a single box, but it is the whole
point: **remote operation becomes almost free — same protocol, different
transport.** The plumbing cost is paid once, up front, instead of being
retrofitted later when local-only assumptions have already hardened.

This directly serves a requirement: local and remote operation must offer the
*same* user experience. A design that special-cased "local" would have to grow a
second path for "remote" and keep the two in sync forever. One socket-based
protocol avoids that split entirely.

## Why the language split

The boundary between Rust and Julia is not arbitrary — each language does what it
is best at.

**Rust is for plumbing.** The TUI, IPC, file watching, process supervision, and
terminal-protocol image rendering all want a single statically-linked,
cross-platform binary with predictable performance. `tokio`, `notify`, `winit`,
and `wgpu` cover that stack. Keep this layer boring and predictable; it should
rarely need to change when the feature set grows.

**Julia is for everything plugin-extensible and Julia-aware.** `JuliaSyntax.jl`
parses Julia source — reimplementing a Julia parser in Rust is a non-starter, and
it is the parser the language itself uses. More fundamentally, the unique value
proposition here is *multiple dispatch as the plugin mechanism*: a small set of
abstract types whose methods are the extension ABI. That only works in Julia. See
[The Dispatch ABI](../extend/abi.md).

## The serialization seam

The Rust↔Julia boundary is crossed by exactly two generic structs, both carrying
**opaque, kernel-defined payloads**:

```julia
struct TreeNode
    id::String                # opaque to Rust
    label::String
    kind::Symbol              # :module, :function, :pngfile, ...
    has_children::Bool
    badges::Vector{Symbol}    # :stale, :user_edited, :immutable, ...
    payload::Dict{String,Any} # opaque, kernel-defined per kind
end

struct PreviewPayload
    mime::String
    data::Vector{UInt8}
    extras::Dict{String,Any}
end
```

The frontend draws whatever tree the kernel sends and dispatches on `PreviewPayload`'s
`mime` to pick a renderer. It never inspects `id` or `payload`; it never learns
what a `:pngfile` *is*. The consequence is the load-bearing property of the whole
design: **adding a new `FileType` requires zero Rust changes** (and a `Mode` too, once the mode-plugin seam is wired — modes are kernel-hosted today). A plugin
that teaches the kernel to preview a new file format ships entirely in Julia, and
the frontend renders it because the MIME type tells it how. See
[`TreeNode`](@ref) and [`PreviewPayload`](@ref) in the API reference.

## Deployment topology

The architecture's payoff shows up in the standard remote deployment: a Windows
or local frontend driving a Linux backend where the Julia kernel and GPU live.

- The **backend runs as a long-lived daemon on the remote**, supervised by a
  named tmux session so it survives SSH disconnects.
- It listens on a **per-session Unix socket**, not a host-allocated TCP port.
- The **frontend forwards that remote socket to a local one over `ssh -L`** —
  one tunnel, one socket — and speaks the same JSON line protocol it would speak
  to a local backend.
- On reconnect (after a laptop sleep, a wifi flap, or a fresh client), the
  frontend re-attaches by **session id**; the kernel keeps its state — variables,
  loaded modules, in-flight computations — across the gap.

A single daemon can host **one Julia kernel per workspace**, routed by
`workspace_id`, so switching the active project is fast and never tears the kernel
down. tmux itself acts as the session registry — listing, creating, and killing
backends — rather than a second bespoke daemon.

## Where to go next

- [Modes](modes.md) — the switchable nav-tree roots over the same code.
- [Previews](previews.md) — which file types render, and at what fidelity.
- [The Dispatch ABI](../extend/abi.md) — how multiple dispatch is the plugin system.
- [Frontend Rendering](../design/rendering.md) — how previews are drawn into the window.
- [Backend & Sessions](../design/backend.md) — daemon internals and session lifecycle.
- [The Line Protocol](../design/protocol.md) — the JSON wire format between the processes.
