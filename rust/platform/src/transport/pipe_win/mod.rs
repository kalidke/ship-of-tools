//! The Windows named-pipe transport's client half: [`PipeClient`], the bounded connect it comes from, and its
//! `Client` impl. Every server is its owner's (the row socket). It moves bytes; every frame shape is
//! the row socket's.
//!
//! # The I/O slot: one state machine, every direction
//!
//! A `Mutex<SlotState>` (`Idle` / `Pending` / `Closing`) guards an
//! address-stable, MANUAL-RESET-event-backed `OVERLAPPED` — manual, not
//! auto, because Microsoft documents overlapped pipe I/O against
//! manual-reset events and warns that an auto-reset event can hang
//! `GetOverlappedResult(..., TRUE)` when a completion races the wait
//! call. [`IoSlot::submit_and_wait`] resets and issues the OS call and
//! flips the state to `Pending` ALL UNDER ONE LOCK ACQUISITION, then
//! releases the lock before the (possibly long) wait — so
//! [`IoSlot::cancel`], which also takes that lock, can only ever observe
//! `Idle` (latch `Closing`; the next submission refuses before touching
//! the OS) or `Pending` (call `CancelIoEx`, then latch `Closing`). A
//! cancel can never miss a submission that hasn't happened yet, nor land
//! after one has already started reusing the structure. The same check
//! also rejects a SECOND submission while one is already `Pending`,
//! distinctly from `Closing`: `PipeClient`'s `read`/`write_all` take
//! `&self`, and without this two concurrent same-direction callers could
//! both reset and reissue the one shared `OVERLAPPED`, corrupting
//! whichever completed second. Rejecting it
//! (`TransportError::ConcurrentSubmit` at the `PipeClient` boundary, never
//! touching the OS) is what makes `unsafe impl Sync for IoSlot` sound:
//! completion is always consumed by exactly the one thread that got past
//! this check.

#![cfg(windows)]

use super::{Client, TransportError, CONNECT_BOUND};
use crate::host::wide_null;
use std::cell::UnsafeCell;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// A raw Windows `HANDLE`, asserted `Send` AND `Sync`. `Send`: exactly one
/// owner ever calls `CloseHandle` on it, only after every thread using a
/// copy has stopped. `Sync`: the wrapped value is never dereferenced as a
/// pointer — it is an opaque OS handle, passed only to `windows-sys`
/// calls — so reading a `&SendableHandle` from multiple threads at once
/// is just reading a plain integer.
#[derive(Clone, Copy)]
struct SendableHandle(HANDLE);
unsafe impl Send for SendableHandle {}
unsafe impl Sync for SendableHandle {}

mod client;
mod slot;

pub(crate) use client::connect_pipe_path_unchallenged;
pub use client::PipeClient;
use slot::*;
