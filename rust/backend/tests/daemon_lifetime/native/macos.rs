//! macOS: the fixture's authority over a process is a Mach task-control right, which this file does not yet acquire.
//! Until it does, every identity request fails with that cause, so a macOS leg of a case fails at its first step and
//! never passes by skipping; the premise waits for the hosted macOS run (see the lane report).

use std::io;
use std::time::Duration;

pub fn start_ticks(_pid: i32) -> io::Result<u64> {
    Err(unavailable())
}

pub struct Identity {
    pub pid: i32,
    pub created: u64,
    pub label: String,
}

impl Identity {
    pub fn acquire(_pid: i32, _created: Option<u64>, _label: &str) -> io::Result<Identity> {
        Err(unavailable())
    }

    pub fn acquire_own(_pid: i32, _label: &str) -> io::Result<Identity> {
        Err(unavailable())
    }

    pub fn is_own(&self) -> bool {
        false
    }

    pub fn exited(&self, _bound: Duration) -> bool {
        false
    }

    pub fn kill(&self) -> io::Result<()> {
        Err(unavailable())
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "the macOS task-control authority is not established (waits for the hosted run)",
    )
}
