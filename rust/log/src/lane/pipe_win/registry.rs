//! The instance registry: creates every pipe instance and is the one closer of their handles.

use super::*;

/// Create one instance of the voyage's named pipe. `first` must be `true`
/// for EXACTLY the very first instance ever created for this pipe name
/// (see [`PipeServer::bind`]'s squat check); every later instance is
/// created with `first = false`, or — per the continuous-name-hold design
/// — RECYCLED rather than freshly created at all. A fresh owner-only
/// descriptor is built per call: `CreateNamedPipeW` copies what it needs
/// from it at creation time.
pub(super) fn create_pipe_instance(
    name: &[u16],
    first: bool,
    max_instances: u32,
) -> std::io::Result<OwnedHandle> {
    let descriptor = crate::host::owner_protected_pipe_descriptor()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let sa = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    #[allow(clippy::disallowed_methods, reason = "listener: capsule lane pipe: an owner-only DACL, then the identity challenge")]
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS | PIPE_WAIT,
            max_instances,
            READ_BUF_LEN as u32,
            READ_BUF_LEN as u32,
            0,
            &sa,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

/// A registry-verified, temporarily-live view of one instance's raw
/// `HANDLE` — the ONLY way any code in this module may pass a raw
/// SERVER-side instance handle to a Win32 call that submits new work
/// against it (`ConnectNamedPipe`/`ReadFile`/`WriteFile`) or cancels/
/// disconnects it (`CancelIoEx`/`DisconnectNamedPipe`).
///
/// INVARIANT: a handle is DEREFERENCED (passed to any such Win32 call)
/// only while THIS guard proves the registry still considers it live.
/// The guard holds [`InstanceRegistry`]'s `RwLock` READ side for its
/// whole lifetime, and [`InstanceRegistry::close_all`] cannot acquire
/// the WRITE side — and therefore cannot run `CloseHandle` on ANY
/// instance — while even ONE `LiveHandle` (for any id) is outstanding
/// anywhere. Take it, use its `HANDLE` for exactly ONE Win32 call, then
/// drop it immediately: NEVER hold it across [`wait_overlapped`]'s own
/// blocking wait — the kernel already has an established I/O context
/// tied to the handle value used at submission time, so the wait itself
/// needs no further guarding, and holding a guard across it would let a
/// single stalled connection's read/write block `close_all` (and
/// therefore `disconnect_listener`) indefinitely, exactly the "never
/// blocks" contract this module promises elsewhere.
pub(super) struct LiveHandle<'a> {
    _read: RwLockReadGuard<'a, RegistryState>,
    handle: SendableHandle,
}

impl LiveHandle<'_> {
    pub(super) fn get(&self) -> HANDLE {
        self.handle.0
    }
}

/// [`InstanceRegistry`]'s internal state: either open (mapping ids to
/// their currently-live handle) or permanently closed.
pub(super) enum RegistryState {
    Open(HashMap<u64, SendableHandle>),
    /// [`InstanceRegistry::close_all`] has run — permanently; never
    /// reopened.
    Closed,
}

/// The outcome of [`InstanceRegistry::create_and_register`].
pub(super) enum CreateOutcome {
    Created(u64, SendableHandle),
    CreateFailed(std::io::Error),
    /// The registry was already `Closed` — `make` was never called.
    ShuttingDown,
}

/// Every pipe-instance HANDLE this server ever creates is created AND
/// registered here ATOMICALLY (see [`create_and_register`]
/// (Self::create_and_register)), and stays registered — through any
/// number of recycle/reuse cycles, through becoming a live connection,
/// through sitting in `AcceptState::retained_dead` — until
/// [`close_all`](Self::close_all) finds and closes it.
///
/// INVARIANT: every instance handle has exactly one closer, AND a
/// handle is only ever passed to an OS call while a [`LiveHandle`]
/// proves it live (see that type's own doc). In this module's own
/// normal operation (see the module doc's "Continuous name hold"
/// section) the closer is NEVER invoked — an instance is recycled or
/// retained-dead forever, never actually closed, while the server
/// intends to keep accepting. The ONE exception is `close_all`, called
/// EXACTLY once, from [`PipeServer::disconnect_listener`]: it atomically
/// drains every currently-registered id and closes each one directly,
/// then permanently refuses (`create_and_register` reports
/// `ShuttingDown` instead of ever calling its `make` closure) any later
/// creation. Because no OTHER code path ever individually removes an id,
/// there is no "removed here, but the remover assumed someone else would
/// close it" gap: whichever bucket (the accept loop's own pending
/// instance, `recycled`, `retained_dead`, or a live `ConnHandle` in
/// `conns`) an id's instance currently sits in, at the instant
/// `close_all` runs it is found and closed — independent of `conns`'
/// own, unrelated bookkeeping timing.
pub(super) struct InstanceRegistry {
    next_id: AtomicU64,
    state: RwLock<RegistryState>,
}

impl InstanceRegistry {
    pub(super) fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            state: RwLock::new(RegistryState::Open(HashMap::new())),
        }
    }

    /// Create a NEW instance and register it, ATOMICALLY with respect to
    /// [`close_all`](Self::close_all) (Codex round-4 finding 1): both this
    /// method and `close_all` take the SAME lock's WRITE side, so either
    /// `make` runs to completion and its handle is inserted BEFORE
    /// `close_all` can ever observe this registry as fully drained, or
    /// the registry is ALREADY `Closed` and `make` never runs at all —
    /// there is no window where a handle exists, holds (or is about to
    /// recreate) the pipe NAME, and is not yet in the map for
    /// `close_all` to find. `make` is `CreateNamedPipeW` wrapped by the
    /// caller — a local syscall, not expected to block meaningfully —
    /// held under this write lock as a deliberate, rare-and-bounded
    /// exception to this module's usual "never block disconnect_listener"
    /// rule, exactly because instance CREATION and teardown's
    /// `close_all` must never interleave.
    pub(super) fn create_and_register(
        &self,
        make: impl FnOnce() -> std::io::Result<OwnedHandle>,
    ) -> CreateOutcome {
        let mut state = self.state.write().unwrap();
        match &mut *state {
            RegistryState::Closed => CreateOutcome::ShuttingDown,
            RegistryState::Open(map) => match make() {
                Ok(owned) => {
                    let raw = SendableHandle(owned.as_raw_handle() as HANDLE);
                    // From this point, THIS registry is the sole future
                    // closer -- never Rust's own `Drop`.
                    std::mem::forget(owned);
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    map.insert(id, raw);
                    CreateOutcome::Created(id, raw)
                }
                Err(e) => CreateOutcome::CreateFailed(e),
            },
        }
    }

    /// Take a temporarily-live view of `id`'s handle for exactly ONE
    /// Win32 call — see [`LiveHandle`]'s own doc for the invariant this
    /// establishes. `None` if `id` is not currently registered (already
    /// closed by `close_all`).
    pub(super) fn live(&self, id: u64) -> Option<LiveHandle<'_>> {
        let read = self.state.read().unwrap();
        match &*read {
            RegistryState::Open(map) => {
                let handle = *map.get(&id)?;
                Some(LiveHandle { _read: read, handle })
            }
            RegistryState::Closed => None,
        }
    }

    /// The one atomic drain-and-shutdown: close EVERY instance currently
    /// registered — regardless of which of this module's several
    /// buckets currently references it — then permanently transition to
    /// `Closed` (future `create_and_register`/`live` calls report
    /// `ShuttingDown`/`None`). Takes the WRITE side of the SAME lock
    /// `live`/`create_and_register` use, so this cannot run concurrently
    /// with (or interleave into the middle of) either — see
    /// [`LiveHandle`]'s and `create_and_register`'s own docs. Never
    /// blocks on anything but that lock: every entry is one
    /// `CloseHandle`, not a join.
    pub(super) fn close_all(&self) {
        let mut state = self.state.write().unwrap();
        if let RegistryState::Open(map) = &mut *state {
            for (_, handle) in map.drain() {
                unsafe { CloseHandle(handle.0) };
            }
        }
        *state = RegistryState::Closed;
    }
}
