//! A lane's own post-SID identity exchange (ADR 0041 Lifecycle "The
//! challenge", steps 4-5): what request bytes to send once, and how to
//! decode the reply into a `(pid, created)` pair or a foreign/incomplete
//! verdict. Portable. `challenge::exchange_identity` (portable — steps 1-3, the
//! genuine OS calls, are each platform's own job) is the ONLY caller of
//! either trait method, and is called only after step 3 (identity
//! equality) has already succeeded — implementing [`IdentityExchange`]
//! cannot skip or reorder the OS authentication steps, because this
//! trait never sees the connection until they are done.

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
    /// this decision lives in the lane, not in `challenge::exchange_identity`.
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
    /// `wire::FrameSplitter`) is itself stateful across calls.
    fn feed(&mut self, bytes: &[u8]) -> ExchangeDecode;
}
