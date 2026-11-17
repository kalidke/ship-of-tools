//! The chat's pictures owner: the thumbnails of the image files the view places, asked for one at a time per host,
//! decoded within bounds on the UI thread and held downscaled, at most `MAX_HELD`. It returns the requests to send and
//! sends nothing itself; only `ChatDrawer` calls it. It also holds each held picture's texture, made at the picture's first
//! draw and gone with its entry, and the one original the person opens in the preview, until State hands it to the bounded child.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use image::{ImageReader, Limits, RgbaImage};
use sot_protocol::chat::{ChatFile, PICTURE_MIMES};
use sot_protocol::ops::{ChatCode, ChatFileReq, ChatFileVariant, ChatTarget};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatPicture, ChatReply, OutgoingReq};
use crate::ui::render::quad::{Quad, ScreenRect};

use super::owner::{Out, LIMITED_PACE, PACE};
use super::view::Tile;

/// Thumbnail requests in flight per host: one, so a daemon's two transfer permits (chat guarantee 32) always leave one for
/// a row's file post while a window loads pictures.
const MAX_IN_FLIGHT: usize = 1;
/// The pictures held or wanted, apart from those in flight; past it the least recently placed leaves first. A frame places
/// at most this many, so a placed picture never pushes out another placed in the same frame.
pub(super) const MAX_HELD: usize = 64;
/// The files refused for the window session; past it the oldest refusal is forgotten (and asked again if placed again).
pub(super) const MAX_FINAL: usize = 1024;
/// The decoder's bounds for one thumbnail: its sides, checked before any pixel buffer, and what it may allocate.
const MAX_SIDE: u32 = 2048;
const MAX_ALLOC: u64 = 32 * 1024 * 1024;
/// A held picture's long side.
const HELD_SIDE: u32 = 360;
/// A `failed` thumbnail is asked again at most this many times, `PACE` apart.
const RETRIES: u8 = 3;

/// A picture's identity: the host it was read from and its record's workspace, channel and file id.
type Key = (HostKey, String, String, String);

/// A held picture: its size, and its pixels until the frame that first draws it makes its texture, which then holds them.
struct Picture {
    size: (u32, u32),
    texture: Texture,
}

enum Texture {
    Unmade(RgbaImage),
    Made(Quad),
    /// The GPU refused it: the tile stays an empty box, and no frame tries again.
    Failed,
}

enum Stand {
    /// Asked for at `due` or later; `failed` is how many `failed` answers came before.
    Wanted { due: Instant, failed: u8 },
    Flight { failed: u8 },
    Held(Picture),
}

struct Entry {
    stand: Stand,
    /// The watch it was last placed under: a request names this target.
    target: ChatTarget,
    /// The placement it was last placed in (`ChatPictures::placement`).
    placed: u64,
    /// Whether its record said Slack had a thumbnail, as last placed.
    thumbnail: bool,
}

pub(super) struct ChatPictures {
    entries: HashMap<Key, Entry>,
    /// The files refused for the window session, never asked again while known; `true` for a file whose `failed` asks ran
    /// out while its record said Slack had no thumbnail: placed again with the thumbnail (its record's edit), it is wanted
    /// again.
    /// it is wanted again. `order` is oldest first, for `MAX_FINAL`.
    finals: HashMap<Key, bool>,
    order: VecDeque<Key>,
    /// The count of placements so far: the last one's entries are the ones asked for.
    placement: u64,
    connected: HashSet<HostKey>,
    /// Hosts whose daemon does not know `chat.file`; cleared when the host's link changes.
    unsupported: HashSet<HostKey>,
    /// The decode (`decode`; a test's stand-in).
    decode: fn(&[u8]) -> Option<RgbaImage>,
    /// The original the person last asked to open, until its reply or a row change; and the one original in flight, which
    /// counts against its host's `MAX_IN_FLIGHT` as a thumbnail does. They differ when a newer open replaced the wanted file.
    wanted: Option<(HostKey, ChatFileReq)>,
    opening: Option<(HostKey, ChatFileReq)>,
    /// The original of the wanted file as it came: the host and row it was opened for and its bytes, until State hands them
    /// to the window's bounded child (`take_opened`). The window's own process never decodes them.
    opened: Option<(HostKey, String, Vec<u8>)>,
    /// How many originals have been handed on: each is the child's job of that number, and only the newest one's outcome is
    /// shown, so a picture opened while another is with the child is the one the person sees.
    handed: u64,
}

impl Default for ChatPictures {
    fn default() -> Self {
        let (entries, finals, order, connected, unsupported) = (HashMap::new(), HashMap::new(), VecDeque::new(), HashSet::new(), HashSet::new());
        Self { entries, finals, order, placement: 0, connected, unsupported, decode, wanted: None, opening: None, opened: None, handed: 0 }
    }
}

/// Decodes a thumbnail's bytes once: one of wire's `PICTURE_MIMES` by its own bytes, under `Limits` (`MAX_SIDE`, checked
/// before any pixel buffer, and `MAX_ALLOC` for what the decoder allocates through them), then held downscaled to
/// `HELD_SIDE` on its long side. `None` refuses it. Not bounded by `Limits`: a lossless WebP's prefix-code tables, which
/// the WebP decoder builds outside them; their cost is set by the reply's `MAX_THUMB` (about 100 MB and 70 ms for a
/// hostile 256 KiB file, measured in a release build), one decode at a time.
pub(super) fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let mut r = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    if !r.format().is_some_and(|f| PICTURE_MIMES.contains(&f.to_mime_type())) {
        return None;
    }
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

/// The rectangle a picture of `size` fills in `tile`: fitted, centered and never enlarged past one picture pixel to a screen
/// pixel.
fn fitted_in((w, h): (u32, u32), tile: ScreenRect) -> ScreenRect {
    let by = (tile.w / w.max(1) as f32).min(tile.h / h.max(1) as f32).min(1.0);
    let (w, h) = (w as f32 * by, h as f32 * by);
    ScreenRect { x: tile.x + (tile.w - w) / 2.0, y: tile.y + (tile.h - h) / 2.0, w, h }
}

impl ChatPictures {
    /// The frame placed these records' files under `target` on `host` (no host: nothing is placed). Only their pictures are
    /// asked for, the first `MAX_HELD` of them; a refused one is skipped, unless its retries ran out with no thumbnail and its
    /// record now has one.
    pub(super) fn place<'a>(&mut self, at: Option<(&HostKey, &ChatTarget)>, files: impl IntoIterator<Item = (&'a str, &'a str, &'a ChatFile)>, now: Instant) {
        self.placement += 1;
        let Some((host, target)) = at else { return };
        let mine = files.into_iter().filter(|(ws, ch, f)| *ws == target.workspace && *ch == target.channel.as_str() && f.is_picture());
        for (ws, ch, f) in mine.take(MAX_HELD) {
            let key = (host.clone(), ws.to_string(), ch.to_string(), f.id.clone());
            match self.finals.get(&key) {
                Some(true) if f.thumbnail => {
                    self.finals.remove(&key);
                    self.order.retain(|k| k != &key);
                }
                Some(_) => continue,
                None => {}
            }
            let e = self.entries.entry(key).or_insert_with(|| Entry { stand: Stand::Wanted { due: now, failed: 0 }, target: target.clone(), placed: 0, thumbnail: f.thumbnail });
            (e.placed, e.thumbnail) = (self.placement, f.thumbnail);
            if e.target != *target {
                e.target = target.clone();
            }
        }
        self.bound();
    }

    /// Each host's requests in flight, the original's included.
    fn flying(&self) -> HashMap<HostKey, usize> {
        let mut flying: HashMap<HostKey, usize> = HashMap::new();
        for (k, e) in &self.entries {
            if matches!(e.stand, Stand::Flight { .. }) {
                *flying.entry(k.0.clone()).or_default() += 1;
            }
        }
        if let Some((h, _)) = &self.opening {
            *flying.entry(h.clone()).or_default() += 1;
        }
        flying
    }

    /// The person opens `file` of `target` on `host`: it becomes the one wanted, in place of any other, asked at the next
    /// `asks` once no original is in flight and its host has room. Nothing is asked twice while in flight. On a host whose link
    /// is down, or whose daemon does not know the op, nothing is kept and the words say why, so it is never sent later unasked.
    pub(super) fn open(&mut self, host: &HostKey, target: &ChatTarget, file: &str) -> Result<(), &'static str> {
        if !self.connected.contains(host) {
            return Err("picture not opened: the link to the hub is down");
        }
        if self.unsupported.contains(host) {
            return Err("picture not opened: the hub must be updated to open it");
        }
        self.wanted = Some((host.clone(), ChatFileReq { target: target.clone(), file: file.to_string(), variant: ChatFileVariant::Original }));
        Ok(())
    }

    /// The wanted original is no one's (the row changed): a reply still coming for it is dropped.
    pub(super) fn cancel_open(&mut self) {
        self.wanted = None;
    }

    /// An original's reply, taken only for the one in flight. For the file still wanted, its bytes are kept, undecoded, for
    /// State (`take_opened`), or the words of its refusal are returned; a reply for a file no longer wanted is dropped.
    /// Then the next asks.
    pub(super) fn on_original(&mut self, host: &HostKey, req: &ChatFileReq, reply: ChatReply<ChatPicture>, now: Instant) -> (Vec<Out>, Option<String>) {
        if self.opening.as_ref() != Some(&(host.clone(), req.clone())) {
            return (Vec::new(), None);
        }
        self.opening = None;
        let words = if self.wanted == Some((host.clone(), req.clone())) {
            self.wanted = None;
            match reply {
                ChatReply::Done(p) => {
                    self.opened = Some((host.clone(), req.target.row.clone(), p.bytes));
                    None
                }
                ChatReply::Refused(code) => Some(format!("picture not opened: {}", code.text())),
                ChatReply::Unsupported => {
                    self.unsupported.insert(host.clone());
                    Some("picture not opened: the hub must be updated to open it".into())
                }
            }
        } else {
            None
        };
        (self.asks(now), words)
    }

    /// The checked original of the wanted file, once.
    pub(super) fn take_opened(&mut self) -> Option<(u64, HostKey, String, Vec<u8>)> {
        let (host, row, bytes) = self.opened.take()?;
        self.handed += 1;
        Some((self.handed, host, row, bytes))
    }

    /// Whether the child's job `number` is of the newest original handed on.
    pub(super) fn is_newest(&self, number: u64) -> bool {
        number == self.handed
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

    /// The requests to send now: the wanted original first, when none is in flight, then the thumbnails of the last placement
    /// that are due; each on a connected host that knows the op, at most `MAX_IN_FLIGHT` in flight per host.
    pub(super) fn asks(&mut self, now: Instant) -> Vec<Out> {
        let mut flying = self.flying();
        let mut outs = Vec::new();
        if let (None, Some((h, req))) = (&self.opening, &self.wanted) {
            if self.connected.contains(h) && !self.unsupported.contains(h) && flying.get(h).copied().unwrap_or(0) < MAX_IN_FLIGHT {
                *flying.entry(h.clone()).or_default() += 1;
                self.opening = self.wanted.clone();
                outs.push((h.clone(), OutgoingReq::ChatFile { req: req.clone() }));
            }
        }
        let mut due: Vec<(&Key, &mut Entry)> = self
            .entries
            .iter_mut()
            .filter(|(k, e)| e.placed == self.placement && matches!(e.stand, Stand::Wanted { due, .. } if due <= now) && self.connected.contains(&k.0) && !self.unsupported.contains(&k.0))
            .collect();
        due.sort_by(|a, b| a.0.cmp(b.0));
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

    /// A thumbnail's reply, taken only for a request in flight: decoded and held (a decode that fails or panics is final), or
    /// by its code final, asked again after `PACE` (`failed` at most `RETRIES` times; `busy`, `unavailable`), or after
    /// `LIMITED_PACE` (`limited`). Then the next asks.
    pub(super) fn on_reply(&mut self, host: &HostKey, req: &ChatFileReq, reply: ChatReply<ChatPicture>, now: Instant) -> Vec<Out> {
        let key = (host.clone(), req.target.workspace.clone(), req.target.channel.as_str().to_string(), req.file.clone());
        let decode = self.decode;
        let Some(e) = self.entries.get_mut(&key) else { return self.asks(now) };
        let Stand::Flight { failed } = e.stand else { return self.asks(now) };
        // `Err`: final, `true` when a thumbnail arriving in its record asks again (its retries ran out with none).
        let next = match reply {
            ChatReply::Done(p) => catch_unwind(AssertUnwindSafe(|| decode(&p.bytes))).ok().flatten().map(|image| Stand::Held(Picture { size: image.dimensions(), texture: Texture::Unmade(image) })).ok_or(false),
            ChatReply::Refused(ChatCode::NotAnImage | ChatCode::TooLarge | ChatCode::BadRequest) => Err(false),
            ChatReply::Refused(ChatCode::Busy | ChatCode::Unavailable) => Ok(Stand::Wanted { due: now + PACE, failed }),
            ChatReply::Refused(ChatCode::Limited) => Ok(Stand::Wanted { due: now + LIMITED_PACE, failed }),
            // `failed` (Slack may have no thumbnail yet), and any other code: asked again, a bounded number of times.
            ChatReply::Refused(_) if failed < RETRIES => Ok(Stand::Wanted { due: now + PACE, failed: failed + 1 }),
            ChatReply::Refused(_) => Err(!e.thumbnail),
            ChatReply::Unsupported => {
                self.unsupported.insert(host.clone());
                Ok(Stand::Wanted { due: now, failed })
            }
        };
        match next {
            Ok(stand) => e.stand = stand,
            Err(waits) => self.refuse(key, waits),
        }
        self.bound();
        self.asks(now)
    }

    /// Draws the held pictures of `tiles` (the view's, on `host`): each picture's texture is made by `make` at its first draw, and
    /// `paint` is given it with `body` and the rectangles its tiles show it in, one call a picture, so a picture shown twice is
    /// drawn from one texture in one call. A tile with no held picture is skipped (its box is the view's).
    pub(super) fn draw<E>(
        &mut self,
        host: &HostKey,
        tiles: Vec<Tile>,
        body: ScreenRect,
        make: impl Fn(&RgbaImage) -> Option<Quad>,
        mut paint: impl FnMut(&mut Quad, ScreenRect, &[ScreenRect]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut shown: Vec<(Key, Vec<ScreenRect>)> = Vec::new();
        for t in tiles {
            let key = (host.clone(), t.workspace, t.channel, t.file);
            let Some(Entry { stand: Stand::Held(p), .. }) = self.entries.get(&key) else { continue };
            let fitted = fitted_in(p.size, t.rect);
            match shown.iter_mut().find(|(k, _)| *k == key) {
                Some((_, rects)) => rects.push(fitted),
                None => shown.push((key, vec![fitted])),
            }
        }
        for (key, rects) in shown {
            let Some(Entry { stand: Stand::Held(p), .. }) = self.entries.get_mut(&key) else { continue };
            // The pixels go once the texture is made, or refused.
            p.texture = match std::mem::replace(&mut p.texture, Texture::Failed) {
                Texture::Unmade(image) => make(&image).map_or(Texture::Failed, Texture::Made),
                made_or_failed => made_or_failed,
            };
            if let Texture::Made(quad) = &mut p.texture {
                paint(quad, body, &rects)?;
            }
        }
        Ok(())
    }

    /// `key` is refused for the window session; past `MAX_FINAL` the oldest refusal is forgotten.
    fn refuse(&mut self, key: Key, waits: bool) {
        self.entries.remove(&key);
        if self.finals.insert(key.clone(), waits).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MAX_FINAL {
            if let Some(old) = self.order.pop_front() {
                self.finals.remove(&old);
            }
        }
    }

    /// A link came or went: nothing of that host is in flight, so each thumbnail is wanted again, asked at once when the link
    /// is up; the original in flight and the one wanted are given up (whether one was wanted), never asked again unasked.
    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) -> bool {
        if up {
            self.connected.insert(host.clone());
        } else {
            self.connected.remove(host);
        }
        self.unsupported.remove(host);
        // No reply comes for an original in flight, and one wanted across a link change is given up.
        self.opening = self.opening.take().filter(|(h, _)| h != host);
        let given_up = self.wanted.as_ref().is_some_and(|(h, _)| h == host);
        if given_up {
            self.wanted = None;
        }
        for (_, e) in self.entries.iter_mut().filter(|(k, _)| &k.0 == host) {
            if let Stand::Flight { failed } = e.stand {
                e.stand = Stand::Wanted { due: now, failed };
            }
        }
        given_up
    }

    /// Past `MAX_HELD` entries not in flight, the least recently placed leave.
    fn bound(&mut self) {
        let mut idle: Vec<(u64, Key)> = self.entries.iter().filter(|(_, e)| !matches!(e.stand, Stand::Flight { .. })).map(|(k, e)| (e.placed, k.clone())).collect();
        let over = idle.len().saturating_sub(MAX_HELD);
        idle.sort();
        idle.into_iter().take(over).for_each(|(_, k)| drop(self.entries.remove(&k)));
    }

    /// The size of the held picture of `file` in `channel` on `host`, if any.
    #[cfg(test)]
    pub(super) fn held(&self, host: &str, workspace: &str, channel: &str, file: &str) -> Option<(u32, u32)> {
        match self.entries.get(&(host.to_string(), workspace.to_string(), channel.to_string(), file.to_string()))?.stand {
            Stand::Held(ref p) => Some(p.size),
            _ => None,
        }
    }

    /// How many held pictures have a texture.
    #[cfg(test)]
    pub(super) fn textures(&self) -> usize {
        self.entries.values().filter(|e| matches!(&e.stand, Stand::Held(Picture { texture: Texture::Made(_), .. }))).count()
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
