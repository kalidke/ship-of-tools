//! Windows owner-only security descriptors and the SID lookups behind them.

use super::io_ctx;
use crate::Result;

/// Owns a security descriptor built by `ConvertStringSecurityDescriptorToSecurityDescriptorW`
/// (`LocalAlloc`'d by that API) for exactly as long as the caller needs it
/// live; freed on drop. `pub` (ADR 0041 step 5, widened for the session-pipe
/// hardening fix): `pipe_win.rs`, a sibling module, builds and consumes the
/// pipe-flavored descriptor below through this same type, and `sot-backend`
/// — a different crate entirely — reaches it through the `fsutil::
/// owner_protected_pipe_descriptor` facade (see that re-export in `lib.rs`)
/// to give its `interprocess`-backed session pipe the identical posture.
/// The `sd` field itself stays private either way; `as_ptr` is the one
/// seam into it.
#[cfg(windows)]
pub struct OwnerProtectedDescriptor {
    sd: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl OwnerProtectedDescriptor {
    /// The raw descriptor pointer for a `SECURITY_ATTRIBUTES.lpSecurityDescriptor`
    /// field. Borrowed, not transferred — the returned pointer is valid only
    /// as long as `self` is alive, exactly like `create_dir_protected`'s own
    /// direct use of the (formerly private) `sd` field.
    pub fn as_ptr(&self) -> windows_sys::Win32::Security::PSECURITY_DESCRIPTOR {
        self.sd
    }
}

#[cfg(windows)]
impl Drop for OwnerProtectedDescriptor {
    fn drop(&mut self) {
        if !self.sd.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.sd as windows_sys::Win32::Foundation::HLOCAL);
            }
        }
    }
}

/// This process's own token-user SID, stringified — the shared first half
/// of every owner-protected descriptor this module builds (the directory
/// flavor below, and `pipe_win.rs`'s pipe flavor): same account, same
/// `OpenProcessToken`/`GetTokenInformation(TokenUser)`/`ConvertSidToStringSidW`
/// lookup, same `LocalAlloc`/`CloseHandle` discipline. Only the SDDL ACE
/// that wraps this SID differs between callers. Also the "this account's"
/// half of the ADR 0041 step 6 same-connection challenge (`challenge.rs`):
/// step 3 compares THIS against [`sid_string_from_process`]'s answer for
/// the target.
#[cfg(windows)]
pub(crate) fn token_user_sid_string() -> Result<String> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle needing no close
    // (unlike the token handle `sid_string_from_process` opens and closes).
    sid_string_from_process(unsafe { GetCurrentProcess() })
}

/// The token-user SID, stringified, of an ARBITRARY (already-open) process
/// handle — the same lookup as [`token_user_sid_string`], generalized for
/// the challenge's target-process check: step 3 opens the CANDIDATE
/// server's token (query-only) rather than this process's own, then
/// compares the two strings for equality. `pub(crate)`: `challenge.rs` is
/// the one other caller.
#[cfg(windows)]
pub(crate) fn sid_string_from_process(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::OpenProcessToken;

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io_ctx(std::io::Error::last_os_error(), format_args!("OpenProcessToken")));
    }
    struct TokenGuard(HANDLE);
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let _token_guard = TokenGuard(token);
    sid_string_from_token(token)
}

/// The `TOKEN_USER` SID, stringified, for an ALREADY-OPEN token handle —
/// the `GetTokenInformation(TokenUser)` size-query-then-fetch idiom,
/// shared by both callers above so there is exactly one implementation of
/// it in this crate.
#[cfg(windows)]
fn sid_string_from_token(token: windows_sys::Win32::Foundation::HANDLE) -> Result<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_USER};

    // TOKEN_USER is variable-length: the SID is appended after the fixed
    // struct, so the documented idiom is size-query-then-fetch. A plain
    // `Vec<u8>` buffer only guarantees 1-byte alignment — not enough for a
    // struct holding a pointer field — so the buffer is `u64`-backed.
    let mut needed: u32 = 0;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    // The sizing call FAILS by contract; the only healthy failure is
    // insufficient-buffer with the needed length filled in.
    let sizing = std::io::Error::last_os_error();
    if sizing.raw_os_error()
        != Some(windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER as i32)
        || needed == 0
    {
        return Err(io_ctx(sizing, format_args!("GetTokenInformation sizing")));
    }
    let words = (needed as usize).div_ceil(8);
    let mut buf: Vec<u64> = vec![0u64; words];
    let buf_ptr = buf.as_mut_ptr().cast::<u8>();
    if unsafe { GetTokenInformation(token, TokenUser, buf_ptr.cast(), needed, &mut needed) } == 0 {
        return Err(io_ctx(std::io::Error::last_os_error(), format_args!("GetTokenInformation")));
    }
    let sid = unsafe { (*buf_ptr.cast::<TOKEN_USER>()).User.Sid };

    // SID -> string (LocalAlloc'd by the API; copied out and freed here).
    let mut sid_str: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_str) } == 0 {
        return Err(io_ctx(std::io::Error::last_os_error(), format_args!("ConvertSidToStringSidW")));
    }
    let sid_string = unsafe { pwstr_to_string(sid_str) };
    unsafe {
        LocalFree(sid_str as windows_sys::Win32::Foundation::HLOCAL);
    }
    Ok(sid_string)
}

/// Build an owner-only, protected descriptor from an SDDL ACE's `flags`
/// and `rights` fields — `("OICI", "FA")` for the directory flavor,
/// `("", "FA")` for the pipe flavor (ADR 0041 step 5) — wrapped around
/// `token_user_sid_string()`'s SID as `D:P(A;<flags>;<rights>;;;<sid>)`,
/// via `ConvertStringSecurityDescriptorToSecurityDescriptorW` rather than a
/// hand-assembled ACL: far less code, and the SDDL string doubles as
/// documentation of exactly what is granted.
///
/// An ACE string is SIX fields (`type;flags;rights;object_guid;
/// inherit_object_guid;account_sid`), not five: omitting `OI`/`CI` must
/// leave the `flags` field EMPTY, never delete it outright -- `D:P(A;FA;;;
/// <sid>)` (five fields) is a real bug this signature makes structurally
/// impossible to reintroduce, caught live on the first real Windows run
/// (`ConvertStringSecurityDescriptorToSecurityDescriptorW` failing every
/// `bind` with error 87/`ERROR_INVALID_PARAMETER`, because that shape
/// parses `"FA"` as the ACE's *flags* field and leaves `rights` empty).
#[cfg(windows)]
fn owner_protected_descriptor_with_ace(flags: &str, rights: &str) -> Result<OwnerProtectedDescriptor> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };

    let sid_string = token_user_sid_string()?;
    // D: (DACL) P (protected), one ACE: (A)llow, `flags`, `rights`, for the
    // token-user SID.
    let sddl = format!("D:P(A;{flags};{rights};;;{sid_string})");
    let sddl_wide = wide_null(&sddl);
    let mut sd: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io_ctx(
            std::io::Error::last_os_error(),
            format_args!("ConvertStringSecurityDescriptorToSecurityDescriptorW {sddl:?}"),
        ));
    }
    Ok(OwnerProtectedDescriptor { sd })
}

/// `D:P(A;OICI;FA;;;<sid>)` — object+container inherit, full access, for
/// the voyage staging root (`create_dir_protected`). Behavior UNCHANGED by
/// the ADR 0041 step 5 refactor above: same SDDL string, same steps, now
/// shared with the pipe flavor below rather than duplicated.
#[cfg(windows)]
pub(super) fn owner_protected_descriptor() -> Result<OwnerProtectedDescriptor> {
    owner_protected_descriptor_with_ace("OICI", "FA")
}

/// `D:P(A;FA;;;<sid>)` — full access, NO `OI`/`CI` — for the ADR 0041 step
/// 5 attach-protocol pipe. "Attach protocol" §Security split: a named
/// pipe's DACL gates the two connection ENDS directly; `OI`/`CI` is
/// directory-child-inheritance semantics with no meaning for a pipe object,
/// so it is deliberately absent here rather than copy-pasted from the
/// directory flavor. `SE_DACL_PROTECTED` (the `P` flag) is preserved
/// identically — a permissive ancestor still can never inject ACEs.
///
/// `pub` and re-exported (`fsutil::owner_protected_pipe_descriptor` in
/// `lib.rs`): `sot-backend`'s session pipe — a second, `interprocess`-backed
/// pipe family the daemon binds directly, not through this module — used to
/// carry the Windows default descriptor (`Everyone`/`ANONYMOUS LOGON` read).
/// It now builds its `interprocess::os::windows::security_descriptor::
/// SecurityDescriptor` from THIS SDDL rather than a second copy of it, so
/// the two pipe families share one owner-only posture instead of drifting.
#[cfg(windows)]
pub fn owner_protected_pipe_descriptor() -> Result<OwnerProtectedDescriptor> {
    owner_protected_descriptor_with_ace("", "FA")
}

/// Read a NUL-terminated wide string produced by a Win32 API into an owned
/// `String` (lossy: these are SIDs/SDDL text, never user-facing content
/// where lossy conversion would matter).
#[cfg(windows)]
unsafe fn pwstr_to_string(p: *const u16) -> String {
    let len = (0..).take_while(|&i| *p.add(i) != 0).count();
    let slice = std::slice::from_raw_parts(p, len);
    String::from_utf16_lossy(slice)
}

/// NUL-terminated UTF-16 for an arbitrary Rust string (the SDDL text) —
/// distinct from `wide_verbatim`, which additionally applies path-specific
/// `\\?\` prefixing that would corrupt a non-path string like this one.
#[cfg(windows)]
fn wide_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}
