//! A pipe lane's own post-SID identity exchange (ADR 0041 Lifecycle "The
//! challenge", steps 4-5): what request bytes to send once, and how to
//! decode the reply into a `(pid, created)` pair or a foreign/incomplete
//! verdict. Portable — the wire codec this depends on
//! ([`wire::FrameSplitter`]) already builds and is tested on every
//! platform, so this module's frame-loop precedence logic is exercised by
//! REAL executed tests everywhere, not merely compile-checked on
//! Windows. `challenge::challenge` (Windows-only, since steps 1-3 are
//! genuine Win32 calls) is the ONLY caller of either trait method, and
//! calls them only after step 3 (SID equality) has already succeeded —
//! implementing [`IdentityExchange`] cannot skip or reorder the OS
//! authentication steps, because this trait never sees the connection
//! until they are done.

use crate::wire::{self, DecodedFrame, MgmtReply, MgmtRequest};

/// What one lane's post-SID exchange concluded, fed one chunk of newly
/// read bytes at a time.
pub enum ExchangeDecode {
    /// Not enough bytes yet for this lane's own framing to decide
    /// anything — keep reading.
    Incomplete,
    /// Exactly one well-formed reply, naming the peer's pid and creation
    /// time.
    Identity { pid: u32, created: u64 },
    /// A well-formed WRONG answer, undecodable bytes, a frame over this
    /// lane's own size cap, or MORE than one complete reply/frame in what
    /// should have been a single round trip. An unproven server — this
    /// lane's own framing is the only thing that can tell "one clean
    /// reply" from "trailing protocol corruption", which is exactly why
    /// this decision lives in the lane, not in `challenge::challenge`.
    Foreign,
}

/// A pipe lane's own post-SID identity exchange.
pub trait IdentityExchange {
    /// This lane's one-shot identity-yielding request, written once as
    /// the first content on the connection after SID equality.
    fn encode_request(&self) -> Vec<u8>;
    /// Consume newly-read bytes (appended to whatever this lane's own
    /// decoder already holds from earlier calls) and report what they now
    /// decode to. `&mut self`: a lane's own decoder (e.g. a
    /// [`wire::FrameSplitter`]) is itself stateful across calls.
    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode;
}

/// The voyage mgmt lane's `IdentityExchange`: `status` request,
/// `status_ok` reply — today's only lane. Error takes precedence over a
/// decoded frame, and exactly one complete reply is accepted per
/// exchange: a `StatusOk` bundled with trailing malformed bytes, or two
/// complete replies in one read, is `Foreign`, never the first frame
/// alone (ADR 0041 U0 round-1 finding 5).
#[derive(Default)]
pub struct VoyageMgmtExchange {
    splitter: wire::FrameSplitter,
}

impl IdentityExchange for VoyageMgmtExchange {
    fn encode_request(&self) -> Vec<u8> {
        wire::encode_mgmt_request(&MgmtRequest::Status)
            .expect("MgmtRequest::Status has no fields; encoding cannot fail")
    }

    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode {
        let (frames, err) = self.splitter.feed(bytes);
        // Finding 5: error takes precedence over any frame decoded in the
        // SAME call, and more than one complete frame is corruption too —
        // both checked BEFORE ever looking at frame contents.
        if err.is_some() {
            return ExchangeDecode::Foreign;
        }
        match frames.len() {
            0 => ExchangeDecode::Incomplete,
            1 => match &frames[0] {
                DecodedFrame::MgmtReply(MgmtReply::StatusOk { pid, created, .. }) => {
                    ExchangeDecode::Identity { pid: *pid, created: *created }
                }
                _ => ExchangeDecode::Foreign, // well-formed, wrong opcode
            },
            _ => ExchangeDecode::Foreign, // two-or-more complete replies in one read
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_ok_bytes(pid: u32, created: u64) -> Vec<u8> {
        wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid, created, survival: wire::Survival::Normal }).unwrap()
    }

    #[test]
    fn incomplete_on_a_partial_frame() {
        let mut ex = VoyageMgmtExchange::default();
        let full = status_ok_bytes(1, 2);
        assert!(matches!(ex.feed(&full[..full.len() - 1]), ExchangeDecode::Incomplete));
    }

    #[test]
    fn decodes_exactly_one_clean_status_ok() {
        let mut ex = VoyageMgmtExchange::default();
        let full = status_ok_bytes(123, 456);
        match ex.feed(&full) {
            ExchangeDecode::Identity { pid, created } => {
                assert_eq!(pid, 123);
                assert_eq!(created, 456);
            }
            ExchangeDecode::Foreign => panic!("expected Identity, got Foreign"),
            ExchangeDecode::Incomplete => panic!("expected Identity, got Incomplete"),
        }
    }

    #[test]
    fn two_complete_replies_in_one_feed_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        let mut bytes = status_ok_bytes(1, 2);
        bytes.extend(status_ok_bytes(1, 2));
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn status_ok_followed_by_malformed_bytes_is_foreign_not_proven() {
        // A frame-plus-error in the SAME feed call: the good StatusOk
        // must not mask the trailing corruption (finding 5).
        let mut ex = VoyageMgmtExchange::default();
        let mut bytes = status_ok_bytes(1, 2);
        bytes.extend([0xffu8; 16]); // an unknown magic -- UnknownMagic
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn malformed_first_frame_alone_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        assert!(matches!(ex.feed(&[0xff; 16]), ExchangeDecode::Foreign));
    }

    #[test]
    fn oversized_first_frame_is_foreign() {
        let mut ex = VoyageMgmtExchange::default();
        // SOM0 magic + a length field announcing far more than MAX_BODY_LEN.
        let mut bytes = b"SOM0".to_vec();
        bytes.extend(u32::MAX.to_le_bytes());
        assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    }

    #[test]
    fn good_first_reply_stops_the_exchange_before_a_later_bad_frame_is_ever_seen() {
        // "good-first/bad-second": the good reply decodes and STOPS the
        // exchange (challenge::challenge's read loop never calls `feed`
        // again once it sees `Identity`), so a bad frame arriving after
        // it on the wire is never fed to THIS instance at all. What
        // matters is that nothing in this lane's own decoder would treat
        // that later corruption as benign if it ever were reached -- a
        // FRESH exchange (modeling "the next thing on the wire") fed only
        // the bad bytes is refused, never silently accepted.
        let mut ex = VoyageMgmtExchange::default();
        assert!(matches!(ex.feed(&status_ok_bytes(1, 2)), ExchangeDecode::Identity { pid: 1, created: 2 }));
        let mut ex2 = VoyageMgmtExchange::default();
        assert!(matches!(ex2.feed(&[0xff; 16]), ExchangeDecode::Foreign));
    }

    #[test]
    fn encode_request_is_the_pinned_status_wire_bytes() {
        let ex = VoyageMgmtExchange::default();
        assert_eq!(ex.encode_request(), wire::encode_mgmt_request(&MgmtRequest::Status).unwrap());
    }
}
