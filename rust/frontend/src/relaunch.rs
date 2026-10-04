//! The self-relaunch contract with the launcher (ADR 0017): the sentinel's path and,
//! on Windows, handing the foreground to the next window.

/// Path of the self-relaunch sentinel file (ADR 0017). The build-and-relaunch
/// helper (`scripts/relaunch-sot.ps1`) creates this after a successful
/// `cargo build`; the watcher thread notices it and triggers an exit-75
/// respawn. `relaunch-sot.ps1 -Converge` writes `converge` as the file's
/// content instead of a bare timestamp (ASCII-encoded, no BOM) -- the
/// watcher thread reads it back (BOM-stripped, case-insensitive,
/// leading-whitespace-tolerant `starts_with("converge")`, so a stray BOM
/// from a different writer/encoding doesn't misdecode a converge as a
/// plain relaunch) to pick exit code 76 over 75; any other content
/// (including the plain timestamp) is a normal relaunch.
pub(crate) fn relaunch_sentinel_path() -> Option<std::path::PathBuf> {
    sot_log::host::state_dir::sot_state_dir().map(|d| d.join("relaunch.request"))
}

/// Grant the next process the right to take the OS foreground (Windows only).
///
/// Called by the outgoing FE just before it exits 75 for an ADR-0017
/// self-relaunch. Because this process currently owns the foreground, it is
/// permitted to call `AllowSetForegroundWindow(ASFW_ANY)`, which lifts the
/// foreground lock so the *next* `SetForegroundWindow` from any process is
/// honoured. The relaunched FE issues that call on its first paint
/// (`force_os_foreground`), so the new window comes up focused instead of
/// merely flashing the taskbar. The grant lasts until the next user input,
/// which comfortably covers the restage+respawn down-window.
#[cfg(windows)]
pub(crate) fn allow_next_foreground() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
}

/// Force the window to the OS foreground (Windows only). Returns `true` once
/// our window actually holds the foreground.
///
/// winit's `focus_window()` issues `SetForegroundWindow`, which Windows
/// silently refuses for a process that isn't already the foreground
/// process (the foreground lock) — it just flashes the taskbar instead.
/// After an ADR-0017 self-relaunch the freshly-spawned FE is precisely
/// that: a brand-new process the user hasn't clicked, spawned `-WindowStyle
/// Hidden` from the background supervisor, so the relaunched window comes up
/// behind whatever took foreground during the down-window.
///
/// Escalating sequence, each step covering a case the prior misses:
///  1. `AttachThreadInput` to the current foreground thread, so the OS treats
///     our `SetForegroundWindow` as same-input-queue and honours it.
///  2. `SetWindowPos` HWND_TOPMOST→HWND_NOTOPMOST toggle to force the window
///     to the top of the z-order while attached.
///  3. If still not foreground, `ShowWindow` SW_MINIMIZE→SW_RESTORE — the one
///     transition Windows always lets take the foreground (a brief flicker,
///     but only on the fallback path). No-op on non-Windows targets.
#[cfg(windows)]
pub(crate) fn force_os_foreground(window: &winit::window::Window) -> bool {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
        SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE,
        SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return false;
    };
    let hwnd = h.hwnd.get() as HWND;

    unsafe {
        if GetForegroundWindow() == hwnd {
            return true; // already foreground — nothing to do
        }
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let our_thread = GetCurrentThreadId();
        // Only attach when the foreground belongs to another thread; attaching
        // a thread to itself fails and isn't needed.
        let attached = !fg.is_null()
            && fg_thread != 0
            && fg_thread != our_thread
            && AttachThreadInput(our_thread, fg_thread, 1) != 0;
        ShowWindow(hwnd, SW_SHOW);
        // Toggle topmost to reorder above everything, then drop back so the
        // window doesn't stay pinned over other apps.
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        SetActiveWindow(hwnd);
        SetFocus(hwnd);
        if attached {
            AttachThreadInput(our_thread, fg_thread, 0);
        }
        if GetForegroundWindow() == hwnd {
            tracing::info!(
                attached,
                "force_os_foreground: took foreground via attach/topmost"
            );
            return true;
        }
        // Hard fallback: minimize→restore is the transition Windows always
        // grants the foreground to. Only reached when the above failed.
        tracing::info!(
            attached,
            "force_os_foreground: attach/topmost failed; trying minimize/restore"
        );
        ShowWindow(hwnd, SW_MINIMIZE);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
        let ok = GetForegroundWindow() == hwnd;
        tracing::info!(ok, "force_os_foreground: minimize/restore result");
        ok
    }
}

/// Starts the one-shot relaunch-sentinel watcher thread (ADR 0017): on the
/// sentinel it sets `flag` to 75 or 76 and wakes the window. `resumed` calls it.
pub(crate) fn spawn_watcher(
    sentinel: std::path::PathBuf,
    flag: std::sync::Arc<std::sync::atomic::AtomicU8>,
    waker: std::sync::Arc<winit::window::Window>,
) {
    if let Err(e) = std::thread::Builder::new()
        .name("sot-relaunch-watch".to_string())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(400));
            if sentinel.exists() {
                // Read BEFORE removing: content picks 75 (plain
                // relaunch) vs 76 (converge — relaunch-sot.ps1
                // -Converge). Unreadable/empty content fails
                // open to a plain relaunch.
                // PowerShell 5.1's `-Encoding utf8` (the
                // writer's ASCII path is preferred now, but a
                // stale/foreign writer can still emit one)
                // prepends a UTF-8 BOM (U+FEFF), which
                // `trim_start()` does NOT strip (it's not
                // Unicode whitespace) -- strip it explicitly
                // first so a BOM-prefixed "converge" doesn't
                // decode as a plain relaunch.
                let is_converge = std::fs::read_to_string(&sentinel)
                    .map(|s| {
                        s.trim_start_matches('\u{feff}')
                            .trim_start()
                            .to_ascii_lowercase()
                            .starts_with("converge")
                    })
                    .unwrap_or(false);
                let _ = std::fs::remove_file(&sentinel);
                flag.store(
                    if is_converge { 76 } else { 75 },
                    std::sync::atomic::Ordering::Relaxed,
                );
                waker.request_redraw();
                break;
            }
        })
    {
        tracing::warn!(error = %e, "failed to spawn relaunch watcher");
    }
}
