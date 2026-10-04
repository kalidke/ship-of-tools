//! The daemon's view of a capsule row's run: its phase vocabulary, lifecycle observer and headless client.

pub(crate) mod end;
pub(crate) mod probe;

/// One lifecycle observer per capsule row -- the SINGLE writer of `Workspace::phase`.
pub(crate) mod observer;

/// ADR 0042 amendment (2026-09-07), "a session types into and reads a
/// sibling row": the daemon's own HEADLESS client on a capsule lane — the
/// same [`sot_log::fe_client_io::FeAttachClient`] the frontend's drawer
/// uses, run on the daemon side with no viewport and no user watching.
/// `type_into` takes the pen only long enough to deliver ONE `input` frame
/// and never resizes the pane (ADR 0041's take-on-first-input semantics,
/// applied to a second kind of client); `screen_of` attaches as a pure
/// WATCHER and never takes at all. Platform-neutral: gated the same as
/// `mod runtime` above, so this module simply does not exist on a host
/// that cannot run a capsule row in the first place. (macOS lane,
/// corrected: `FeAttachClient` is NO LONGER what gates this —
/// `sot_log::fe_client_io` is ungated since ADR 0045 decision 1, and only
/// its `PlatformEndpoint`-typed default is cfg'd. The reason was solely
/// `mod runtime`'s own gate, and that gate is gone — so is this one.)
pub(crate) mod headless;
