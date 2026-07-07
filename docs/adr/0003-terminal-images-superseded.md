# ADR 0003: Terminal image protocol

**Status:** SUPERSEDED by `0003-rendering-surface.md` (2026-05-07). Original text retained for the historical record — the autodetect-with-fallback approach inverted the project's premise; see the replacement ADR for the rationale.
**Date:** 2026-05-07

## Context

PNGs and plot images must render inline in the TUI. Terminal image support is fragmented: Kitty graphics (kitty, WezTerm), Sixel (recent Windows Terminal, foot, xterm with patches), iTerm2 protocol (macOS, not in scope), halfblocks (universal but ugly).

Primary user platform is Windows Terminal. Linux secondary. Either can be a remote target later.

## Decision

Use `ratatui-image` with `Picker::from_query_stdio()` autodetection. Honor an env var override `SOT_IMG=kitty|sixel|halfblocks` for cases where autodetect picks wrong.

Cap preview blobs at 2 MB. Downscale large images via `Images.jl` `imresize` in the kernel before sending — keeps bandwidth bounded and avoids terminal-side limits (Windows Terminal has Sixel size constraints we have not measured).

## Consequences

- **Must be re-validated in M0** with a throwaway `rust/scratch/img-probe` binary on the user's actual terminal. If autodetect picks an unacceptable protocol, the env override is the escape hatch.
- Halfblocks fallback is ugly for figures but ensures the system always renders *something*.
- Downscaling in the kernel (not the frontend) keeps Rust-side image handling minimal and centralizes the policy in one Julia file.
- If `ratatui-image` proves insufficient (e.g., Windows Terminal Sixel chokes on real plots), fallback is `icy_sixel` for hand-rolled encoding. Don't lock in until M0 probe passes.
