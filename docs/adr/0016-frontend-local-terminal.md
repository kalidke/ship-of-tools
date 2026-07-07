# ADR 0016: Frontend local terminal pane — PTY, emulator, render, and drawer-state choices

**Status:** Accepted
**Date:** 2026-05-26

## Context

TODO section G adds a bottom-drawer terminal pane (Ctrl-T) that gives the user a live OS shell inside the frontend. The canonical use case is SSHing from the local machine to backend hosts — deliberately not a remote shell proxied through the daemon. Several independent choices were made during design; this ADR records each one explicitly so the implementation decisions are not re-litigated later.

Two constraints shaped all of them:

- **The terminal is a local escape hatch, not a backend feature.** The LLM pane already forwards to a backend tmux session via the transport. The terminal pane is for when the user wants a shell on the frontend machine — or wants to SSH outward from it. It must work even when the backend is unreachable.
- **The drawer is now shared between REPL and Terminal.** Ctrl-J and Ctrl-T select content in the same physical drawer slot. The layout calculation must not conflate "is the drawer open" with "what is showing."

## Decision

### 1. Local frontend PTY, not `pty.open` routed through the backend

The PTY is spawned on the frontend machine using the `portable-pty` crate (already a backend dependency at v0.9; added to `rust/frontend/Cargo.toml`). The terminal pane does not send `pty.open` over the transport and does not require a backend connection.

Rationale: the feature's purpose is local access. Routing a local shell through the backend transport would add a round-trip on every keystroke, break the feature when the backend is down, and complicate the transport with a new mux channel — none of which serves the use case. If a backend-hosted shell is later wanted, it goes through the existing `pty.open` mechanism in the LLM/BL panes; the terminal pane stays local.

Consequences:

- **Positive:** works independently of backend connectivity; no protocol changes; no latency added to the input path.
- **Positive:** keeps the `term/` module entirely self-contained — no dependency on transport types.
- **Negative:** a user wanting a shell *on* the backend must still SSH out manually (which is exactly the intended workflow).

### 2. Reuse `vt100-ctt` for the grid model; do not add `alacritty_terminal` or `wezterm-term`

The frontend already depends on `vt100-ctt 0.17`. The LLM pane is already driven by a `vt100` grid. The terminal pane uses the same emulator.

Rationale: one emulator, one mental model. Adding a second emulator crate (`alacritty_terminal` or `wezterm-term`) to handle the same class of escape sequences introduces two codepaths that can diverge in behavior and doubles the surface area to maintain. The test for switching crates is concrete: if a real-world program (vim, tmux, less, ssh) fails in the terminal pane due to a missing escape sequence that `vt100-ctt` does not implement, we revisit then — not speculatively now.

Consequences:

- **Positive:** no new crate dependencies; no second emulator state machine to reason about.
- **Positive:** LLM pane and terminal pane share the same grid-rendering and diff logic.
- **Escape hatch:** if `vt100-ctt` proves inadequate in practice (e.g. vi mode in bash, 256-color mouse-tracking, sixel), migrate both panes together to a single richer crate. That migration is easier when done once than when the two panes have diverged onto different emulators.

### 3. Reuse the existing `paint_terminal` render path (cosmic-text grid) for v1

The terminal grid is painted via the same `paint_terminal` function already used by the LLM pane — cosmic-text cell grid, one glyph per cell, fixed-width layout. No new per-glyph wgpu-quad cell renderer is introduced for v1.

Rationale: the principled per-glyph quad renderer (TODO "Open user-reported items / Per-glyph terminal rendering") is a shared refactor that benefits the LLM pane, the REPL pane, and the terminal pane together. Shipping the terminal pane should not block on that refactor, and the refactor should not be done in isolation for only one pane. The known cursor cell↔glyph drift caveat (documented in the open-items list) is inherited, accepted for v1, and resolved once by the shared follow-up.

Consequences:

- **Positive:** terminal pane ships without a new rendering subsystem; existing render tests cover it.
- **Known limitation:** cursor positioning can drift slightly when a glyph is wider than the cell grid expects. Accepted for v1; the per-glyph quad path is the shared fix.
- **Implementation note:** the per-glyph refactor, when it lands, should target the `paint_terminal` abstraction level so all three panes (LLM, REPL, Terminal) migrate together.

### 4. Drawer becomes `enum { Closed, Repl, Terminal }`, replacing `drawer_open: bool`

The current `state.drawer_open: bool` is promoted to `state.drawer: DrawerContent` with three variants. The keybindings are symmetric:

| Key | Drawer was Closed | Drawer was showing this pane | Drawer was showing the other pane |
|---|---|---|---|
| Ctrl-J | → Repl | → Closed | → Repl |
| Ctrl-T | → Terminal | → Closed | → Terminal |

`layout::compute` continues to receive a plain `bool` (`drawer != DrawerContent::Closed`) — which physical drawer slot to render and which content to paint are render concerns, not layout concerns. The layout module stays ignorant of what is in the drawer.

Consequences:

- **Positive:** symmetric keybinding model is easy to describe to users and to implement (no special cases).
- **Positive:** layout geometry is unchanged; no reflow regressions.
- **Call sites in `gpu.rs`:** `drawer_open: bool` reads become `matches!(self.drawer, DrawerContent::Closed)` checks; the `Ctrl-J` handler is split into a three-branch match; `Ctrl-T` is a new parallel handler with the same shape.
- **`state-<hostname>.toml` persistence:** the drawer enum serializes as a string (`"closed"`, `"repl"`, `"terminal"`); existing files with `drawer_open = false/true` map to `"closed"` / `"repl"` on first read for backward compatibility.

### 5. Reader-thread wakeup via a passed closure

The async reader task in `term/` signals the winit event loop that new terminal output has arrived by calling a `request_redraw` closure passed in at spawn time. The closure signature is `Arc<dyn Fn() + Send + Sync>`.

Rationale: this mirrors how the transport task calls `Arc<Window>::request_redraw()` cross-thread today. It keeps the `term/` module free of winit imports and event-loop types — the module takes a generic wakeup callback and does not care whether the caller is winit, a test harness, or something else.

Consequences:

- **Positive:** `term/` is independently testable without a winit event loop.
- **Positive:** consistent with the established pattern in the transport task; no new mechanism to learn.
- **Cost:** the closure must be `Send + Sync`; types captured in it must be too. In practice this means `Arc<Window>` (already `Send + Sync`) or a `tokio::sync::Notify`.

## What is deferred

- **G7 — Selection and copy.** Mouse-drag text selection within the terminal grid and Ctrl+Shift+C copy via `arboard`. Shares design with "Copy text out of the LLM pane" (same grid abstraction, same clipboard path). Deferred until the LLM pane copy work is also ready, so the two are designed together.
- **Per-glyph wgpu-quad cell renderer.** The shared follow-up described in decision 3. Out of scope for the initial terminal pane; tracked in the open-items list.
- **Multiplexing and tabs.** A single PTY per drawer slot. Multiple simultaneous shells, tab bars, split panes within the terminal are not in scope for v1 and are not architecturally blocked by these decisions.

## Frontend API surface

```rust
// gpu.rs
enum DrawerContent { Closed, Repl, Terminal }
struct State { …, drawer: DrawerContent, term: Option<TermState> }

// term/mod.rs
pub struct TermState {
    pty: Box<dyn MasterPty>,
    parser: vt100::Parser,
    writer: Box<dyn Write + Send>,
}
pub fn spawn(
    cols: u16, rows: u16,
    shell: &str,
    on_data: Arc<dyn Fn() + Send + Sync>,
) -> Result<TermState>;
pub fn write_input(state: &mut TermState, bytes: &[u8]);
pub fn paint_term(
    state: &TermState,
    font_system: &mut FontSystem,
    rect: Rect,
    buffer: &mut Buffer,
);

// settings.rs addition
struct TerminalSettings { shell: Option<String> }  // [terminal] shell override
```
