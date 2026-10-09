//! birth_claim.rs — the claim a capsule's birth carries: the row's own `supervisor.lock`, taken before the birth is
//! accepted and kept, by the same open file description, through the native launch, the target's exec and the new
//! supervisor's takeover. There is one fence per row and no second lock, receipt or reclaim; this is a facade over
//! `host::lock_supervisor_for_handover` that pins the file name the way [`super::journal::fence`] does.
//!
//! The claim is close-only (`host::HandoverLock`): the process that takes it passes the descriptor to the child
//! through the launcher and drops its own copy; the fence stays contended until the last copy closes, whether the
//! parent closed it, died, or the child exited.

#![cfg(unix)]

use crate::host;
use crate::Result;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::path::Path;

/// What a supervisor born holding a claim is told about it: the descriptor of the claim and the one-use channel it
/// answers on once the claim is its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InheritedBirth {
    pub claim_fd: RawFd,
    pub takeover_fd: RawFd,
}

/// The takeover record a new supervisor writes once the claim is its own: its pid and its start identity, so the
/// parent that still holds a copy of the claim can check it is the supervisor it forked before it lets go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Takeover {
    pub pid: u32,
    pub created: u64,
}

const TAKEOVER_MAGIC: [u8; 4] = *b"SOTK";
/// The size of a takeover record on the wire.
pub const TAKEOVER_LEN: usize = 16;

impl Takeover {
    fn encode(self) -> [u8; TAKEOVER_LEN] {
        let mut out = [0u8; TAKEOVER_LEN];
        out[..4].copy_from_slice(&TAKEOVER_MAGIC);
        out[4..8].copy_from_slice(&self.pid.to_le_bytes());
        out[8..].copy_from_slice(&self.created.to_le_bytes());
        out
    }

    /// The record in `bytes`, or `None` when it is not exactly one.
    pub fn decode(bytes: &[u8]) -> Option<Takeover> {
        if bytes.len() != TAKEOVER_LEN || bytes[..4] != TAKEOVER_MAGIC {
            return None;
        }
        Some(Takeover {
            pid: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            created: u64::from_le_bytes(bytes[8..].try_into().ok()?),
        })
    }
}

/// The fence of `state_dir`, held for a birth.
pub struct BirthClaim {
    lock: host::HandoverLock,
}

impl BirthClaim {
    /// Take the fence under `state_dir` (bootstrapping it when absent). `Error::State` when another holder has it:
    /// a live authority, or another birth's claim.
    pub fn take(state_dir: &Path) -> Result<BirthClaim> {
        let path = super::journal::fence::supervisor_lock_path(state_dir);
        host::lock_supervisor_for_handover(&path).map(|lock| BirthClaim { lock })
    }

    /// The descriptor the launcher leaves open in the child.
    pub fn as_raw_fd(&self) -> RawFd {
        self.lock.as_raw_fd()
    }

    /// The new supervisor's takeover of the claim it was born holding (`fd`).
    pub fn adopt(fd: RawFd, state_dir: &Path) -> Result<BirthClaim> {
        let path = super::journal::fence::supervisor_lock_path(state_dir);
        host::HandoverLock::adopt(fd, &path).map(|lock| BirthClaim { lock })
    }
}

/// Write the takeover record on `fd` (the one-use channel the parent gave this supervisor) and close it.
pub fn acknowledge_takeover(fd: RawFd, takeover: Takeover) -> std::io::Result<()> {
    // SAFETY: the supervisor was handed this descriptor for this one use and nothing else owns it.
    let mut channel = unsafe { std::fs::File::from_raw_fd(fd) };
    channel.write_all(&takeover.encode())
}

/// Read one takeover record from `channel`: `Ok(None)` at EOF with nothing written (the supervisor ended or let go of
/// the channel without taking the claim over).
pub fn read_takeover(channel: &mut impl Read) -> std::io::Result<Option<Takeover>> {
    let mut buf = [0u8; TAKEOVER_LEN];
    let mut have = 0;
    while have < buf.len() {
        match channel.read(&mut buf[have..])? {
            0 if have == 0 => return Ok(None),
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "a torn takeover record",
                ))
            }
            n => have += n,
        }
    }
    Takeover::decode(&buf).map(Some).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "not a takeover record")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_takeover_record_round_trips_and_a_torn_or_foreign_one_is_refused() {
        let record = Takeover {
            pid: 4242,
            created: 987_654_321,
        };
        let mut wire: &[u8] = &record.encode();
        assert_eq!(read_takeover(&mut wire).unwrap(), Some(record));
        assert_eq!(
            read_takeover(&mut &[][..]).unwrap(),
            None,
            "EOF with nothing written is no record"
        );
        let full = record.encode();
        assert!(
            read_takeover(&mut &full[..7]).is_err(),
            "a torn record is an error"
        );
        let mut foreign = full;
        foreign[0] = b'X';
        assert!(
            read_takeover(&mut &foreign[..]).is_err(),
            "a record without the magic is refused"
        );
        assert_eq!(Takeover::decode(&full[..15]), None);
    }
}
