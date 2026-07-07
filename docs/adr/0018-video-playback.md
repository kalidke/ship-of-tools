# ADR 0018: Video — poster in the pane, playback in the browser

**Status:** Accepted (revised 2026-05-27; supersedes the original in-pane-playback design recorded earlier the same day)
**Date:** 2026-05-27

## Context

Phase 1 deferred video to "thumbnail only via shelled-out ffmpeg." We first built full in-pane playback (backend ffmpeg decode → JPEG frames over the wire → a frontend `VideoPlayer` with a playback clock). In practice that looked **bad and choppy**: frames were re-encoded to JPEG and downscaled, pulled in batches over the SSH wire with no real buffering, and software-paced off a wall clock. There's no way to match a native player that way.

How VS Code does it: an HTML5 `<video>` element in its Electron webview hands the *real compressed stream* to the platform's **hardware decoder**, which does GPU-accelerated decode, buffering, and frame pacing. We can't beat that with software MJPEG-over-SSH.

**Decision (user-directed):** stop trying to play video in the pane. The preview pane shows a **single poster still**; **`o` opens the real file in the OS browser** (HTML5 `<video>`, native HW decode) for actual viewing. This also fits the established "rich/interactive content lives in the browser" policy (Pluto, WGLMakie) — video playback is just another browser pop-out.

## Decision

### 1. Poster in the pane via the existing image path

`VideoFile.preview()` (plugin `ShipToolsVideoFile`) shells `ffmpeg -ss 0 -i <f> -frames:v 1 -vcodec png` and returns the frame as **`image/png`**. The frontend's existing PNG/image quad path renders it unchanged — no video-specific frontend code. ffmpeg-missing / extraction failure returns a `text/markdown` note (not a blank pane). This deletes the entire in-pane-playback stack (see "Removed").

### 2. `o` → browser, served over a forwarded loopback HTTP port

The file lives on the backend host (often a remote host); the browser is local. So this mirrors the Pluto `o`-open path:

- **Backend HTTP file server** (`rust/backend/src/http_serve.rs`): a small hand-rolled tokio HTTP/1.1 server on `127.0.0.1:<videoPort>` (default 1235, `SOT_VIDEO_PORT`). Serves **only** video-extension files, with **byte-range (`206`) support** — essential for `<video>` seeking. Spawned once at backend startup. Hand-rolled rather than pulling the axum/hyper/tower tree into an otherwise HTTP-free daemon; the need is narrow (GET one file + single `Range`).
- **`video.open {path}` op** (backend `handle_video_open`) returns `http://127.0.0.1:<videoPort><abs-path>`.
- **Launcher** (`scripts/launch-devenv.ps1`) SSH-forwards `<videoPort>` alongside the Pluto port.
- **Frontend**: `o` on a video extension (NavTree) dispatches `OutgoingReq::VideoOpen`; `IncomingEvt::VideoOpened { Ok(url) }` hands the URL to the existing `open_url_in_browser()`.

### 3. Backend preview cap exemption (retained from the first cut)

`try_plugin_preview` skips the FileType plugin for inputs >2 MiB (the assumption that plugin output scales with input). That's false for video — the poster is bounded regardless of file size — so video extensions are exempted (`is_streaming_media`), else large videos get no poster. The bytes-level fallback also returns an informative stub for video (plugin-not-loaded) rather than shipping the raw container.

## Removed (was the in-pane-playback design)

Deleted as part of this revision: the core `video_frames` ABI; the plugin's `video_frames` + MJPEG splitter; the kernel `video.frames` op; the frontend `preview/video.rs` `VideoPlayer`, the `application/vnd.devenv.video+json` mime + branch, the `VideoFrames`/`VideoError` transport, the playback clock / `about_to_wait` animation / transport controls / title scrub bar, and `--capture-video-frame`.

## Consequences

- **Positive:** playback is native quality + smoothness (HW decode, real buffering, seeking via HTTP range); the pane stays cheap (one still through the existing image path); a large amount of bespoke playback code is gone; no heavy HTTP dependency added.
- **Negative:** playback leaves the TUI (a browser window) — consistent with the browser-content policy but not "everything in one surface." Requires the forwarded port to be up (launcher handles it). The hand-rolled HTTP server is minimal (single-range, `Connection: close`); fine for `<video>`, not a general web server.

## Verification

- Backend: `cargo` build; curl the running server — HEAD returns `200` + exact `Content-Length`; `Range: bytes=0-99` returns `206` + `Content-Range`; non-video → `403`; missing → `404`. (Verified on a 16 MiB mp4.)
- Kernel: `file.preview` on a video returns `image/png` poster (PNG magic verified).
- Live (Windows): poster shows in the pane; `o` opens the video in the browser and plays/seeks natively. (Pending Windows-side rebuild + relaunch so the forwarded port is up.)

## Out of scope / deferred
- In-pane playback (removed by design).
- Serving non-video files over the HTTP port.
- Audio is the browser's concern now (it just works in `<video>`).
