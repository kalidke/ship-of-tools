// sot-protocol
//
// Shared types for the JSON line protocol between frontend, backend, and kernel.
// Wire format: NDJSON envelopes — one JSON object per line, UTF-8, `\n`-terminated.
// Blob payloads are length-prefixed binary frames following an envelope whose
// payload contains `"blob": {"len": N, "mime": "…"}`. See docs/adr/0001.
//
// The IR types (`TreeNode`, `PreviewPayload`) mirror the Julia types in
// `core/src/ConceptExplorerCore.jl` so the same JSON shape works on both
// sides of the Rust↔Julia seam.

pub mod codec;
pub mod ir;
// ADR 0045 decision 3: `DaemonLaneEndpoint` — the attach client's own
// `sot_log::client::Endpoint` for the lane bridge (`lane.connect`),
// alongside the platform pipe/socket endpoints `sot-log` itself owns.
// Lives here, not in `sot-log`, because it is a WIRE client of this
// crate's own `LaneConnectReq`/`LaneConnectRes` op — `sot-log` has no
// dependency on `sot-protocol` to build against.
pub mod lane_client;
pub mod ops;
pub mod session_socket;

pub use codec::{read_frame, write_frame};
pub use ir::{BlobDescriptor, PreviewPayload, TreeNode};
pub use ops::{
    op, AgentSendReq, AgentSendRes, ClientVersion, ConceptListRes, ConceptReadReq, ConceptReadRes,
    ConceptWriteReq, ConceptWriteRes, DaemonVersion, DirectoryEntry, DirectoryListReq,
    DirectoryListRes, DocsOpenReq, DocsOpenRes, FeCommandEvt, FeCommandSendReq, FeCommandSendRes,
    FePresenceReq, FePresenceRes, FileChunk, FileDeleteReq, FileDeleteRes, FileDownloadReq,
    FileReadReq, FileReadRes,
    FileUploadAck, FileUploadReq, FileWriteReq, FileWriteRes, GpuSample, HelloReq, HelloRes,
    HostLatest, HostSeries, ImageCropReq, ImageCropRes, KernelRequestReq, LaneConnectReq,
    LaneConnectRes, MathRenderReq, MathRenderRes, MonitorHistoryReq, MonitorHistoryRes,
    MonitorSample, MonitorSubscribeReq,
    MonitorSubscribeRes, MonitorTickEvt, MonitorUnsubscribeReq, PlutoOpenReq, PlutoOpenRes,
    PreviewGetReq, PreviewGetRes, PreviewSetScaleReq, ProcSample, ProxyConnectReq, ProxyConnectRes,
    PtyCursor, PtyEvt, PtyInputReq, PtyInputRes, PtyOpenReq, PtyOpenRes, PtyResizeReq,
    PtyScreenReq, PtyScreenRes, PtyScrollReq, PtyWriteReq, QuartoOpenReq, QuartoOpenRes,
    ReplErrorOut, ReplEvalReq, ReplEvalRes, ReplExecuteInput, ReplExecuteReq, ReplExecuteRes,
    ReplFrame, ReplFrameEvt, ReplRunFileReq, ReplRunFileRes, ReplValueOut, StackFrame,
    TmuxCapturePaneReq, TmuxCapturePaneRes, TmuxCreateSessionReq, TmuxKillSessionReq,
    TmuxListPanesReq, TmuxListPanesRes, TmuxListSessionsRes, TmuxPane, TmuxSession,
    ToggleHiddenReq, ToggleHiddenRes, TreeChildrenReq, TreeChildrenRes, TreeRootReq, TreeRootRes,
    UpdateApplyReq, UpdateApplyRes, UpdateCheckReq, UpdateCheckRes, VersionQueryReq,
    VersionQueryRes, VideoOpenReq, VideoOpenRes, WorkspaceActivateReq, WorkspaceActivateRes,
    WorkspaceCreateReq, WorkspaceCreateRes, WorkspaceDestroyReq, WorkspaceDestroyRes,
    WorkspaceListEntry, WorkspaceListReq, WorkspaceListRes,
};
// `is_private_dir` stays module-private to `session_socket` (not
// re-exported here): nothing outside that module calls it directly
// (`runtime_sot_dir` is its only caller), so a crate-root re-export would
// be dead weight -- codex follow-up, ADR 0042 L2b.
pub use session_socket::{current_uid, runtime_sot_dir, session_socket_path, slug};

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// Product version embedded at build time (ADR 0030 §1, §8 decisions 31a
/// and 31c). Exactly one form means "this is an official release build":
/// the bare `X.Y.Z`, emitted only when the build was produced by the release
/// workflow, sits exactly on its release tag `vX.Y.Z`, and had a clean tree.
/// Everything else is marked: `X.Y.Z+src` for a build off a tag that CI did
/// not produce (or one built with no git at all), `X.Y.Z-dev+<sha>` for an
/// ordinary clean dev build, `X.Y.Z-dev+<sha>-dirty` when the tree was
/// dirty.
///
/// **Do not test this string to decide policy** — call [`is_release_build`],
/// which answers the same question from the flags directly. The string is
/// for humans and for the version stamp; a substring test on it is how the
/// `contains("-dev")` guards used to let a clean on-tag source build through
/// as if it were a release.
///
/// `+src` is semver BUILD METADATA, which is ignored for ordering (see
/// `sot_updater::semver`), so a marked build compares equal to the release
/// it was built from and can never present itself as newer or older.
///
/// The `-dirty` suffix is a SNAPSHOT taken when `build.rs` last ran, not a
/// live-tracked flag (see that file's own doc on `SOT_BUILD_DIRTY`) — an
/// edit made after the last build can leave it stale until something else
/// forces a rebuild. It is a hint for a dev build, never a promise; the
/// actual provenance GUARANTEE this string leans on is a clean, exactly-
/// on-tag release build, which never carries this suffix at all (a tree
/// on-tag but ALSO dirty is not that release, so it still falls through to
/// the `-dirty` form rather than collapsing to the bare tag version). Two
/// distinct dirty trees at one HEAD still alias on this sha — deferred,
/// same as `rust/log/build.rs` already documents for its own build id.
pub fn app_version() -> String {
    format_app_version(
        env!("CARGO_PKG_VERSION"),
        env!("SOT_BUILD_SHA"),
        env!("SOT_BUILD_ON_TAG") == "1",
        env!("SOT_BUILD_DIRTY") == "1",
        env!("SOT_BUILD_ORIGIN") == "ci",
    )
}

/// Whether this binary is an official release artifact — the single
/// authority for every "may this install self-update?" decision (ADR 0030
/// §8 decision 31c).
///
/// Policy lives here rather than in a test on the version string. The two
/// agree today — the bare version is emitted on exactly this condition, and
/// a unit test holds them to it — but that equivalence is a property of the
/// current string shape, not something a caller should have to know. The
/// previous guards did have to know it, spelled `contains("-dev")`, and a
/// clean checkout parked on a release tag defeated them: it builds the same
/// source as the release, so it printed the same bare `X.Y.Z`, passed as a
/// release install, and took updates the launcher's dev-pair-first rule
/// then discarded. The box "updated" and silently stayed put.
///
/// Fails closed: anything it cannot prove is not a release.
pub fn is_release_build() -> bool {
    is_release(
        env!("SOT_BUILD_ON_TAG") == "1",
        env!("SOT_BUILD_DIRTY") == "1",
        env!("SOT_BUILD_ORIGIN") == "ci",
    )
}

/// The shared predicate behind [`is_release_build`] and the bare-version
/// branch of [`format_app_version`], stated once so the string and the
/// policy can never drift apart: the version is bare if and only if this is
/// true.
fn is_release(on_tag: bool, dirty: bool, ci: bool) -> bool {
    ci && on_tag && !dirty
}

/// The pure formatting core of [`app_version`], pulled out so each shape is
/// unit-tested without needing distinct compile-time `env!` values (those
/// are baked in once per build, so `app_version()` itself can only ever
/// exercise ONE branch per test run).
fn format_app_version(pkg: &str, sha: &str, on_tag: bool, dirty: bool, ci: bool) -> String {
    if is_release(on_tag, dirty, ci) {
        return pkg.to_string();
    }
    // No git (a source tarball): nothing can be proven about this tree, and
    // it is not the CI build above, so it is marked like any other
    // unverifiable source build rather than borrowing the bare version.
    if sha.is_empty() {
        return format!("{pkg}+src");
    }
    if dirty {
        format!("{pkg}-dev+{sha}-dirty")
    } else if on_tag {
        // Clean and on the tag, but not from CI: the same source as the
        // release, a different artifact. `+src` says exactly that.
        format!("{pkg}+src")
    } else {
        format!("{pkg}-dev+{sha}")
    }
}

/// One-line `--version` output: `<bin> <version> (<sha> <date>)`, or
/// `<bin> <version>` when built without git.
pub fn version_line(bin: &str) -> String {
    let sha = env!("SOT_BUILD_SHA");
    let date = env!("SOT_BUILD_DATE");
    if sha.is_empty() {
        format!("{bin} {}", app_version())
    } else {
        format!("{bin} {} ({sha} {date})", app_version())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Req,
    Res,
    Evt,
}

/// Wire envelope. Payload is left as `serde_json::Value` so the codec can
/// route on `op` and inspect `payload.blob` without baking every op into a
/// single enum — keeps the protocol crate small and lets new ops land
/// without churning shared structs.
///
/// `rev`, when set, carries the session revision the frame represents. Per
/// ADR 0010 the frontend tracks the highest seen revision and feeds it into
/// the next `hello` so the backend can replay events the client missed.
/// Events from the replay path always carry `rev`; bare control responses
/// may carry it too when the op mutated session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub v: u32,
    pub id: u64,
    pub kind: Kind,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
    pub payload: serde_json::Value,
}

impl Frame {
    pub fn req(id: u64, op: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            kind: Kind::Req,
            op: op.to_string(),
            rev: None,
            payload,
        }
    }
    pub fn res(id: u64, op: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            kind: Kind::Res,
            op: op.to_string(),
            rev: None,
            payload,
        }
    }
    pub fn evt(op: &str, payload: serde_json::Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: 0,
            kind: Kind::Evt,
            op: op.to_string(),
            rev: None,
            payload,
        }
    }
    /// Stamp this frame with the session revision it represents.
    pub fn with_rev(mut self, r: u64) -> Self {
        self.rev = Some(r);
        self
    }
}

#[cfg(test)]
mod app_version_tests {
    use super::{format_app_version, is_release};

    // Argument order throughout: (pkg, sha, on_tag, dirty, ci).

    #[test]
    fn dirty_build_differs_from_clean_at_the_same_sha() {
        let clean = format_app_version("0.6.0", "abc1234", false, false, false);
        let dirty = format_app_version("0.6.0", "abc1234", false, true, false);
        assert_eq!(clean, "0.6.0-dev+abc1234");
        assert_eq!(dirty, "0.6.0-dev+abc1234-dirty");
        assert_ne!(clean, dirty, "a dirty build must never alias a clean one's version string");
    }

    #[test]
    fn dirty_on_tag_still_shows_dirty_not_the_bare_tag() {
        // A tree that sits ON the release tag but carries local edits is not
        // the release it claims to be — must not collapse to the bare
        // `X.Y.Z` a clean on-tag build reports. True even in CI: a dirty
        // release build is not a release.
        assert_eq!(
            format_app_version("0.6.0", "abc1234", true, true, false),
            "0.6.0-dev+abc1234-dirty"
        );
        assert_eq!(
            format_app_version("0.6.0", "abc1234", true, true, true),
            "0.6.0-dev+abc1234-dirty"
        );
    }

    #[test]
    fn clean_on_tag_is_the_bare_version_only_from_ci() {
        // THE regression this whole predicate exists for. Same source, same
        // sha, same clean tree, on the same tag — the only difference is who
        // built it, and that difference must be visible.
        let ci = format_app_version("0.6.0", "abc1234", true, false, true);
        let local = format_app_version("0.6.0", "abc1234", true, false, false);
        assert_eq!(ci, "0.6.0");
        assert_eq!(local, "0.6.0+src");
        assert_ne!(
            ci, local,
            "a local build of a tagged tree must never alias the release artifact"
        );
    }

    #[test]
    fn no_git_is_marked_src() {
        // Source tarball: no sha, so nothing about the tree can be proven,
        // and it does not get to borrow the bare version. `build.rs` cannot
        // find a tag without git either, so on_tag is false in every
        // reachable no-git build (rust/protocol/build.rs) — including under
        // CI, which is why there is no bare-version case here to test.
        assert_eq!(format_app_version("0.6.0", "", false, false, false), "0.6.0+src");
        assert_eq!(format_app_version("0.6.0", "", false, true, false), "0.6.0+src");
        assert_eq!(format_app_version("0.6.0", "", false, false, true), "0.6.0+src");
    }

    #[test]
    fn the_bare_version_and_the_release_predicate_never_disagree() {
        // The string is documentation; `is_release` is policy. They are
        // derived from one condition precisely so a future edit cannot let
        // a build print the bare version while the updater refuses it, or
        // the reverse — which is the failure mode that produced this fix.
        for &on_tag in &[true, false] {
            for &dirty in &[true, false] {
                for &ci in &[true, false] {
                    let bare = format_app_version("0.6.0", "abc1234", on_tag, dirty, ci)
                        == "0.6.0";
                    assert_eq!(
                        bare,
                        is_release(on_tag, dirty, ci),
                        "bare-version and is_release disagree at on_tag={on_tag} dirty={dirty} ci={ci}"
                    );
                }
            }
        }
    }
}
