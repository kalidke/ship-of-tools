//! The chat's pictures owner: the thumbnails of the image files the view places, asked for at most two at a time per host,
//! decoded within bounds on the UI thread and held downscaled, at most `MAX_HELD`. It returns the requests to send and
//! sends nothing itself; only `ChatDrawer` calls it.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::time::Instant;

use image::{ImageFormat, ImageReader, Limits, RgbaImage};
use sot_protocol::chat::ChatFile;
use sot_protocol::ops::{ChatCode, ChatFileReq, ChatFileVariant, ChatTarget};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatPicture, ChatReply, OutgoingReq};

use super::owner::{Out, LIMITED_PACE, PACE};

/// The file types the chat shows as pictures (the plan's D8; the daemon refuses any other `not_an_image`).
const PICTURES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// Thumbnail requests in flight per host.
const MAX_IN_FLIGHT: usize = 2;
/// The pictures held, apart from those in flight; past it the least recently placed leaves first. A frame places at most
/// this many, so a placed picture never pushes out another placed in the same frame.
pub(super) const MAX_HELD: usize = 64;
/// A thumbnail's header is read first: a side past this is never decoded.
const MAX_SIDE: u32 = 2048;
/// What the decoder may allocate for one thumbnail.
const MAX_ALLOC: u64 = 32 * 1024 * 1024;
/// A held picture's long side.
const HELD_SIDE: u32 = 360;
/// A `failed` thumbnail is asked again at most this many times, `PACE` apart.
const RETRIES: u8 = 3;

/// A picture's identity: the host it was read from and its record's workspace, channel and file id.
type Key = (HostKey, String, String, String);

enum Stand {
    /// Asked for at `due` or later; `failed` is how many `failed` answers came before.
    Wanted { due: Instant, failed: u8 },
    Flight { failed: u8 },
    Held(RgbaImage),
    /// Refused for the window session: never asked again while it is known.
    Final,
}

struct Entry {
    stand: Stand,
    /// The watch it was last placed under: a request names this target.
    target: ChatTarget,
    /// The placement it was last placed in (`ChatPictures::placement`).
    placed: u64,
}

#[derive(Default)]
pub(super) struct ChatPictures {
    entries: HashMap<Key, Entry>,
    /// The count of placements so far: the last one's entries are the ones asked for.
    placement: u64,
    connected: HashSet<HostKey>,
    /// Hosts whose daemon does not know `chat.file`; cleared when the host's link changes.
    unsupported: HashSet<HostKey>,
}

/// Whether a record's file is one the chat shows as a picture.
pub(super) fn is_picture(f: &ChatFile) -> bool {
    PICTURES.contains(&f.mime.as_str())
}

/// Decodes a thumbnail's bytes: one of the four formats, its header within `MAX_SIDE` before any pixel is read, the decode
/// within `Limits`, then held downscaled to `HELD_SIDE` on its long side. `None` refuses it.
pub(super) fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let reader = || ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok().filter(|r| matches!(r.format(), Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP)));
    let (w, h) = reader()?.into_dimensions().ok()?;
    if w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    let mut r = reader()?;
    let mut limits = Limits::default();
    (limits.max_image_width, limits.max_image_height, limits.max_alloc) = (Some(MAX_SIDE), Some(MAX_SIDE), Some(MAX_ALLOC));
    r.limits(limits);
    let rgba = r.decode().ok()?.to_rgba8();
    let long = rgba.width().max(rgba.height());
    if long <= HELD_SIDE {
        return Some(rgba);
    }
    let fit = |side: u32| ((u64::from(side) * u64::from(HELD_SIDE) / u64::from(long)) as u32).max(1);
    Some(image::imageops::thumbnail(&rgba, fit(rgba.width()), fit(rgba.height())))
}

impl ChatPictures {
    /// The frame placed these records' files under `target` on `host` (no host: nothing is placed). Only their pictures are
    /// asked for, the first `MAX_HELD` of them.
    pub(super) fn place<'a>(&mut self, at: Option<(&HostKey, &ChatTarget)>, files: impl IntoIterator<Item = (&'a str, &'a str, &'a ChatFile)>, now: Instant) {
        self.placement += 1;
        let Some((host, target)) = at else { return };
        let mine = files.into_iter().filter(|(ws, ch, f)| *ws == target.workspace && *ch == target.channel.as_str() && is_picture(f));
        for (ws, ch, f) in mine.take(MAX_HELD) {
            let key = (host.clone(), ws.to_string(), ch.to_string(), f.id.clone());
            let e = self.entries.entry(key).or_insert_with(|| Entry { stand: Stand::Wanted { due: now, failed: 0 }, target: target.clone(), placed: 0 });
            (e.target, e.placed) = (target.clone(), self.placement);
        }
        self.bound();
    }

    /// Each host's requests in flight.
    fn flying(&self) -> HashMap<HostKey, usize> {
        let mut flying: HashMap<HostKey, usize> = HashMap::new();
        for (k, e) in &self.entries {
            if matches!(e.stand, Stand::Flight { .. }) {
                *flying.entry(k.0.clone()).or_default() += 1;
            }
        }
        flying
    }

    /// When an ask is next owed: the earliest `due` of the last placement's wanted pictures on a connected host that knows the
    /// op and has room in flight, so the window wakes for it; `None` when none would go.
    pub(super) fn next_ask(&self) -> Option<Instant> {
        let flying = self.flying();
        self.entries
            .iter()
            .filter(|(k, e)| e.placed == self.placement && self.connected.contains(&k.0) && !self.unsupported.contains(&k.0) && flying.get(&k.0).copied().unwrap_or(0) < MAX_IN_FLIGHT)
            .filter_map(|(_, e)| match e.stand {
                Stand::Wanted { due, .. } => Some(due),
                _ => None,
            })
            .min()
    }

    /// The thumbnails to ask for now: those of the last placement that are due, on a connected host that knows the op, at most
    /// `MAX_IN_FLIGHT` in flight per host.
    pub(super) fn asks(&mut self, now: Instant) -> Vec<Out> {
        let mut flying = self.flying();
        let mut due: Vec<(&Key, &mut Entry)> = self
            .entries
            .iter_mut()
            .filter(|(k, e)| e.placed == self.placement && matches!(e.stand, Stand::Wanted { due, .. } if due <= now) && self.connected.contains(&k.0) && !self.unsupported.contains(&k.0))
            .collect();
        due.sort_by(|a, b| a.0.cmp(b.0));
        let mut outs = Vec::new();
        for (k, e) in due {
            let n = flying.entry(k.0.clone()).or_default();
            if *n >= MAX_IN_FLIGHT {
                continue;
            }
            *n += 1;
            let Stand::Wanted { failed, .. } = e.stand else { continue };
            e.stand = Stand::Flight { failed };
            outs.push((k.0.clone(), OutgoingReq::ChatFile { req: ChatFileReq { target: e.target.clone(), file: k.3.clone(), variant: ChatFileVariant::Thumb } }));
        }
        outs
    }

    /// A thumbnail's reply, taken only for a request in flight: decoded and held, or by its code final, asked again after
    /// `PACE` (`failed` at most `RETRIES` times; `busy`, `unavailable`), or after `LIMITED_PACE` (`limited`). Then the next
    /// asks.
    pub(super) fn on_reply(&mut self, host: &HostKey, req: &ChatFileReq, reply: ChatReply<ChatPicture>, now: Instant) -> Vec<Out> {
        let key = (host.clone(), req.target.workspace.clone(), req.target.channel.as_str().to_string(), req.file.clone());
        let Some(e) = self.entries.get_mut(&key) else { return self.asks(now) };
        let Stand::Flight { failed } = e.stand else { return self.asks(now) };
        e.stand = match reply {
            ChatReply::Done(p) => decode(&p.bytes).map_or(Stand::Final, Stand::Held),
            ChatReply::Refused(ChatCode::NotAnImage | ChatCode::TooLarge | ChatCode::BadRequest) => Stand::Final,
            ChatReply::Refused(ChatCode::Busy | ChatCode::Unavailable) => Stand::Wanted { due: now + PACE, failed },
            ChatReply::Refused(ChatCode::Limited) => Stand::Wanted { due: now + LIMITED_PACE, failed },
            // `failed`, and any other code: asked again, a bounded number of times.
            ChatReply::Refused(_) if failed < RETRIES => Stand::Wanted { due: now + PACE, failed: failed + 1 },
            ChatReply::Refused(_) => Stand::Final,
            ChatReply::Unsupported => {
                self.unsupported.insert(host.clone());
                Stand::Wanted { due: now, failed }
            }
        };
        self.bound();
        self.asks(now)
    }

    /// A link came or went: nothing of that host is in flight, so each is wanted again, asked at once when the link is up.
    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) {
        if up {
            self.connected.insert(host.clone());
        } else {
            self.connected.remove(host);
        }
        self.unsupported.remove(host);
        for (_, e) in self.entries.iter_mut().filter(|(k, _)| &k.0 == host) {
            if let Stand::Flight { failed } = e.stand {
                e.stand = Stand::Wanted { due: now, failed };
            }
        }
    }

    /// Past `MAX_HELD` entries not in flight, the least recently placed leave.
    fn bound(&mut self) {
        let mut idle: Vec<(u64, Key)> = self.entries.iter().filter(|(_, e)| !matches!(e.stand, Stand::Flight { .. })).map(|(k, e)| (e.placed, k.clone())).collect();
        let over = idle.len().saturating_sub(MAX_HELD);
        idle.sort();
        idle.into_iter().take(over).for_each(|(_, k)| drop(self.entries.remove(&k)));
    }

    /// The held picture of `file` in `channel` on `host`, if any.
    #[cfg(test)]
    pub(super) fn held(&self, host: &str, workspace: &str, channel: &str, file: &str) -> Option<&RgbaImage> {
        match self.entries.get(&(host.to_string(), workspace.to_string(), channel.to_string(), file.to_string()))?.stand {
            Stand::Held(ref p) => Some(p),
            _ => None,
        }
    }

    /// How many pictures are held.
    #[cfg(test)]
    pub(super) fn held_count(&self) -> usize {
        self.entries.values().filter(|e| matches!(e.stand, Stand::Held(_))).count()
    }
}

#[cfg(test)]
#[path = "pictures_tests.rs"]
mod tests;
