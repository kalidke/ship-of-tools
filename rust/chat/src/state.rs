//! A delivering channel's state: the boundary, below which every post was filed or passed, in a file of its own, and the
//! set of filings above it, in memory. The inboxes are the record of what was filed, so the set is merged from the
//! filings the caller reads back from them at every start and is never written; the boundary is never taken from a
//! filing. The caller owns the folder and the clock; this module reads and writes only the one file named from a
//! validated workspace and channel.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use sot_protocol::chat::Ts;

use crate::config::is_slack_id;
use crate::mail::Filed;
use crate::normalize::ts_key;

/// The filings one channel's set holds above its boundary before the caller is told (about 4 MB). Soft: nothing above the
/// boundary is ever dropped, since a dropped filing would be filed again.
pub const MAX_FILED: usize = 100_000;

/// The most a state file may hold: a `Ts` and its line end are 28 bytes, so this ends a stall and bounds a planted file.
const MAX_STATE: u64 = 4096;

/// Why a channel has no state: a code, never a path or a byte of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    /// A workspace or channel is not a Slack id, so no file is named.
    BadId,
    /// The file exists and cannot be read or is not exactly one `Ts`: the channel stops, the file is never read as missing.
    Unreadable,
    /// The boundary could not be written durably.
    Unwritable,
}

/// The file's text: the boundary alone, on one line.
pub fn encode(boundary: &Ts) -> String {
    format!("{}\n", boundary.as_str())
}

/// The boundary a file holds: one `Ts` and its line end, and nothing else.
pub fn decode(text: &str) -> Result<Ts, StateError> {
    Ts::parse(text.strip_suffix('\n').ok_or(StateError::Unreadable)?).map_err(|_| StateError::Unreadable)
}

/// The channel's file in `dir`, named from the two validated ids alone.
pub fn path(dir: &Path, workspace: &str, channel: &str) -> Result<PathBuf, StateError> {
    if is_slack_id(workspace) && is_slack_id(channel) {
        Ok(dir.join(format!("delivery-{workspace}-{channel}")))
    } else {
        Err(StateError::BadId)
    }
}

/// Writes the boundary through platform's durable write.
pub fn write(dir: &Path, workspace: &str, channel: &str, boundary: &Ts) -> Result<(), StateError> {
    sot_platform::host::write_durable(&path(dir, workspace, channel)?, encode(boundary).as_bytes()).map_err(|_| StateError::Unwritable)
}

/// One channel's boundary and its filed set.
#[derive(Debug)]
pub struct State {
    workspace: String,
    channel: String,
    boundary: Ts,
    filed: BTreeSet<(u128, u32)>,
}

impl State {
    /// The channel's state: the later of the file's boundary and the channel's `floor`, and an empty set. A missing file
    /// is the floor; a file that cannot be read or parsed is `Unreadable`.
    pub fn load(dir: &Path, workspace: &str, channel: &str, floor: &Ts) -> Result<State, StateError> {
        let boundary = match sot_platform::host::read_regular(&path(dir, workspace, channel)?, MAX_STATE) {
            Ok(text) => decode(&text)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => floor.clone(),
            Err(_) => return Err(StateError::Unreadable),
        };
        let boundary = if ts_key(&boundary) >= ts_key(floor) { boundary } else { floor.clone() };
        Ok(State { workspace: workspace.into(), channel: channel.into(), boundary, filed: BTreeSet::new() })
    }

    pub fn boundary(&self) -> &Ts {
        &self.boundary
    }

    /// Adds the filings of this channel above the boundary; true when this took the set past `MAX_FILED`. A filing of
    /// another channel or workspace, or at or below the boundary, is not taken.
    pub fn merge<'a>(&mut self, filings: impl IntoIterator<Item = &'a Filed>) -> bool {
        let mut over = false;
        for f in filings {
            if f.workspace == self.workspace && f.channel == self.channel {
                over |= self.insert(&f.ts);
            }
        }
        over
    }

    /// A filing's `ts` joins the set, unless it is at or below the boundary; true when this took the set past
    /// `MAX_FILED`, so the caller logs it once.
    pub fn insert(&mut self, ts: &Ts) -> bool {
        ts_key(ts) > ts_key(&self.boundary) && self.filed.insert(ts_key(ts)) && self.filed.len() == MAX_FILED + 1
    }

    /// Whether `ts` is settled: at or below the boundary every post was filed or passed, and above it the set holds it.
    pub fn contains(&self, ts: &Ts) -> bool {
        ts_key(ts) <= ts_key(&self.boundary) || self.filed.contains(&ts_key(ts))
    }

    pub fn count(&self) -> usize {
        self.filed.len()
    }

    /// Moves the boundary to `to` if that is later, and drops every filing at or below it; it never lowers it.
    pub fn commit(&mut self, to: &Ts) {
        if ts_key(to) > ts_key(&self.boundary) {
            self.boundary = to.clone();
        }
        let key = ts_key(&self.boundary);
        self.filed.retain(|k| *k > key);
    }
}
