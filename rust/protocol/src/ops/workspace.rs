// rows and accounts: workspace.create, list, activate, destroy, reauth, accounts.list

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCreateReq {
    /// Human-friendly label (project_root basename is fine). Used to
    /// derive the workspace's slug and `sot-be-<slug>` tmux session.
    pub label: String,
    /// Absolute path. The daemon will use this as the workspace's
    /// `--project=` for kernels and as the file walker's root.
    pub project_root: String,
    /// True if the FE should launch claude on first attach to this
    /// workspace's session. `#[serde(default)]` so existing callers /
    /// JSON without the field still deserialize (defaults false).
    #[serde(default)]
    pub autostart_claude: bool,
    /// Which agent to auto-start in the workspace pane (ADR 0031):
    /// "claude" | "codex" | "none". Empty (absent on the wire) derives
    /// from `autostart_claude` for back-compat.
    #[serde(default)]
    pub agent: String,
    /// The sot-comm handle the spawned agent should join as. Optional
    /// on the wire (`#[serde(default)]` → empty string when absent), so
    /// existing callers / JSON without the field still deserialize.
    #[serde(default)]
    pub agent_name: String,
    /// The initial instruction the FE delivers to the spawned agent after
    /// auto-starting claude. Optional on the wire (`#[serde(default)]` →
    /// empty string when absent).
    #[serde(default)]
    pub task: String,
    /// ADR 0042's rule (ADR 0043 decision 22, flipped by L6 / this
    /// repo's B6 lane now that the bridge — ADR 0045 — gives a capsule
    /// row a remote attach path; ADR 0046 decision 5 closes the last
    /// gap): which runtime hosts this workspace's agent pane — `""`
    /// (absent on the wire; `#[serde(default)]`) means this host's own
    /// platform default, which is `"capsule"` on every host where the
    /// capsule runtime compiles (`cfg(any(windows, target_os =
    /// "linux"))`) and `"tmux"` only where it doesn't (macOS, for now);
    /// `"capsule"` still asks for a capsule row explicitly on either
    /// platform. `"tmux"` is refused wherever the capsule runtime
    /// compiles — Windows AND, as of decision 5, Linux too (the no-knob
    /// rule: nothing NEW runs on tmux on a capsule-capable host; existing
    /// tmux rows keep running and retire by attrition) — and accepted
    /// only on a host with no capsule runtime at all (macOS).
    #[serde(default)]
    pub runtime: String,
    /// Accounts brief (v0.6.0): which discovered account this row's
    /// agent should run under — the name of a subdirectory of
    /// `.claude-auth` in this daemon's own home, or `None`/empty for the
    /// agent's own default config folder. Claude only this release —
    /// Codex accounts are deferred, so a codex row with a non-default
    /// account is refused. Resolved once, here, at create, and recorded
    /// on the row; refused loudly (never a silent fallback to the
    /// default folder) if the named subdirectory does not exist, or if
    /// this is a bash (`agent == "none"`) row — a bash row has no account
    /// to spend. `#[serde(default)]` so existing callers / JSON without
    /// the field still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    // NOTE (ADR 0023 §3): the daemon-boot trigger travels as an extra wire field
    // `boot: bool` on this op's payload, but is intentionally NOT a struct field
    // here — `handle_workspace_create` reads it straight off the raw JSON. Adding
    // it to the struct would force the FE's `WorkspaceCreateReq { … }` literal
    // (net/transport/ops/workspace.rs) to set it, and the FE is frozen during the sot-names rename.
    // serde ignores the unknown field on this typed deserialize, so the contract
    // stays additive. Fold `boot` into the struct once the FE is unfrozen.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCreateRes {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    pub session_name: String,
}

/// `workspace.reauth` (ADR 0046 decision 6): which row, which account it
/// should spend from now on, and which conversation the replacement leg
/// resumes. `resume` is REQUIRED with no default — a resume id is the only
/// honest selector across an account switch (`--continue` reads a
/// per-account, never-shared `.claude.json`), so an absent one is a
/// refusal, not a fallback. The caller reads the id out of its own
/// environment (`CLAUDE_CODE_SESSION_ID`); nothing persists it.
/// The daemon refuses an id whose transcript was not started in this row's
/// root (`resume_not_this_row`), so no client can make a daemon resume one
/// of its rows' conversations in another of its rows. That holds only for
/// rows the one-root gate of `workspace.create` kept apart: a transcript
/// started in this row's directory by anything else passes (a row of another
/// daemon sharing this home's `projects`, a terminal or headless claude run
/// there, a row from a toml older than the gate, or the same directory
/// through a bind mount).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceReauthReq {
    pub workspace_id: String,
    pub account: String,
    pub resume: String,
}

/// The accept. `code` is always `"reauth_accepted"`; `account` is the
/// account's NAME, for a human to read — `"default"` for the default
/// login, which the record itself stores as `""`. A REFUSAL is not this shape — it is the ordinary
/// `{error, code, accounts}` payload every other op refuses with, and
/// carries the discovered account names so the caller never re-implements
/// discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceReauthRes {
    pub code: String,
    pub workspace_id: String,
    pub account: String,
}

/// `workspace.list` has no fields — the daemon always returns its full
/// in-memory registry. Kept as a struct so future filters (kernel-only,
/// running-only) can land additively.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceListReq {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceListEntry {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    /// The row's stored session name, the target used to attach its pane; it need not be derived from the slug.
    pub session_name: String,
    /// True if the workspace's `Kernel` handle has been constructed —
    /// i.e. some op has caused the daemon to lazily instantiate it.
    /// Reflects in-memory state only; if the underlying Julia child has
    /// died silently the daemon won't notice until the next op.
    pub kernel_running: bool,
    /// True iff this workspace is the daemon's default (the one ops
    /// resolve to when no `workspace_id` is supplied). The frontend can
    /// use this to mark the row and to avoid switching away from the
    /// implicit anchor.
    pub is_default: bool,
    /// True if the FE should launch claude on first attach to this
    /// workspace's session. `#[serde(default)]` so a daemon that predates
    /// the field (e.g. mid-rollout, not yet restarted) still deserializes.
    #[serde(default)]
    pub autostart_claude: bool,
    /// Which agent this workspace auto-starts (ADR 0031): "claude" |
    /// "codex" | "none". The FE renders a per-agent sigil from this.
    #[serde(default)]
    pub agent: String,
    /// The sot-comm handle the spawned agent should join as. These
    /// (with `task`) let the FE deliver the bootstrap straight off
    /// workspace.list — no fe-inbox correlation. Empty string = unset.
    #[serde(default)]
    pub agent_name: String,
    /// The sot-comm handle the session inside this workspace actually
    /// declared to this daemon via `agent.join` (ADR 0046 decision 1) —
    /// distinct from `agent_name` above, which is only the handle the
    /// workspace was CREATED to expect. Empty string = never joined.
    /// `#[serde(default)]` so a daemon that predates the field still
    /// deserializes.
    #[serde(default)]
    pub agent_handle: String,
    /// The initial instruction the FE delivers to the spawned agent after
    /// auto-starting claude. Empty string = unset.
    #[serde(default)]
    pub task: String,
    /// Owning-agent work-state surfaced from the sot-comm registry
    /// (`comm-status.sh` writes `.agents[<agent_name>]`). The daemon reads the
    /// registry fresh on each `workspace.list` and copies these through because
    /// the FE runs on a separate machine/HOME and can't read it directly. One
    /// of "working" | "idle" | "blocked" | "done"; empty string when the
    /// registry / agent / field is absent (no agent_name, no registry yet, …).
    #[serde(default)]
    pub agent_state: String,
    /// One-liner the owning agent set alongside `agent_state` (the `summary`
    /// field in the registry). Empty string when absent.
    #[serde(default)]
    pub agent_summary: String,
    /// ISO8601 timestamp the registry recorded for the state (`status_at`).
    /// Empty string when absent.
    #[serde(default)]
    pub agent_status_at: String,
    /// Lifecycle of the workspace's persistent REPL child (mirrors the
    /// `lifecycle` repl.frame evt, for FEs that (re)connect mid-boot): one of
    /// "not_started" (no child yet — the pre-first-eval norm), "starting"
    /// (spawned, precompiling/booting — NOT dead), "ready" (serve loop up),
    /// "dead" (child exited; respawns on next eval). Empty string from a
    /// daemon that predates the field (`#[serde(default)]`, mid-rollout).
    /// Unlike `kernel_running` (the *Kernel* handle), this reflects the
    /// user-code REPL — the process whose first boot looks dead without it.
    #[serde(default)]
    pub repl_state: String,
    /// ADR 0042 slice L1a: `"tmux"` | `"capsule"` — which runtime hosts
    /// this workspace's agent pane. `#[serde(default)]` so a daemon that
    /// predates the field still deserializes; the empty string never
    /// occurs on the wire from an L1a-or-later daemon (every entry has a
    /// real value), so an old FE reading a blank default here is exactly
    /// as informative as reading nothing.
    #[serde(default)]
    pub runtime: String,
    /// The capsule's own state directory (host-local absolute path),
    /// present only for `runtime == "capsule"` rows — the frontend
    /// attaches to it directly (ADR 0041 U3's client; L1b). Absent for
    /// `"tmux"` rows: `skip_serializing_if` keeps their wire shape
    /// byte-for-byte unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
    /// The capsule's supervisor-lane phase (ADR 0041 Lifecycle: STARTING |
    /// READY | ENDING | ENDED-NO-RESPAWN | TERMINAL, snake_case), `"stopped"`
    /// when its state directory was never created (no supervisor has ever
    /// run for it), `"unreachable"` when the lane could not be queried at
    /// all, or `"foreign"` (ADR 0030 §8 decision 31c) when it WAS queried
    /// and answered — but refused this daemon's own build
    /// (`sot_log::identity::exchange::SUPERVISOR_LANE_BUILD_ID` mismatch,
    /// `version_skew`): a row this daemon can never attach, adopt, end, or
    /// destroy. Distinct from `"unreachable"` (no answer at all) even
    /// though both start from the same failed query — present only for
    /// `runtime == "capsule"` rows, same reasoning as `state_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Detail of the most recent FAILED start-on-attach activation, kept
    /// until the next attempt. Never implies `phase == "terminal"`.
    /// Present only for `runtime == "capsule"` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
    /// Accounts brief (v0.6.0): the discovered account this row's agent
    /// runs under, `""` for the agent's own default config folder (the
    /// common case — kept a plain `String`, never `Option`, so an older
    /// daemon's `#[serde(default)]` empty string and "explicitly
    /// default" are the same value, never two). The sessions list
    /// appends `·<name>` only when this is non-empty.
    #[serde(default)]
    pub account: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceListRes {
    pub workspaces: Vec<WorkspaceListEntry>,
}

/// `accounts.list` has no fields — the daemon always discovers and
/// returns every account in its own home fresh, at request time (no
/// declaration to filter by). Kept as a struct, same convention as
/// `WorkspaceListReq`, so an additive filter can land later without a
/// wire-shape break.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountsListReq {}

/// One discovered account (accounts brief, v0.6.0): a name plus which
/// agent kinds had a folder found for it, and — for those same kinds
/// only — whether that folder's own credentials file exists.
/// `logged_in` is DISPLAY-ONLY information (the picker, `sotd status`):
/// it never gates `workspace.create`, which refuses solely on the named
/// folder being absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountEntry {
    pub name: String,
    pub kinds: Vec<String>,
    pub logged_in: std::collections::BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountsListRes {
    /// Sorted with "default" first, then the rest alphabetically.
    pub accounts: Vec<AccountEntry>,
}

/// `workspace.activate` request. See `op::WORKSPACE_ACTIVATE` for who sends
/// this and why. `None` = the default workspace, same convention as
/// `TreeRootReq::workspace_id`; accepted as either a workspace_id or a slug
/// (the backend's `resolve` takes either).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceActivateReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// A person switched the view to this workspace (Sessions-Enter,
    /// Shift+Left/Right cycling) rather than the frontend or an agent
    /// moving the view programmatically. An old frontend omits this field
    /// (defaults to `false`); an old daemon ignores it. See ADR 0044.
    #[serde(default)]
    pub read: bool,
}

/// `workspace.activate` response. `workspace_id` is the CANONICAL id the
/// daemon resolved `req.workspace_id` to (present even when the request
/// named a slug, or named nothing and resolved to the default workspace).
/// `None` when it didn't resolve to anything live — the frontend does not
/// act on this today, but a future version could surface it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceActivateRes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// `workspace.destroy` request — identify the target workspace by id or
/// slug (the backend's `resolve` accepts either). The default workspace's
/// ROW is never destroyable — it's the daemon's anchor, seeded on boot,
/// with no fallback target to swap ops to. A default TMUX row's request
/// is flatly refused (`code = "default_workspace_not_destroyable"`). A
/// default CAPSULE row instead ends its capsule run and keeps the row —
/// its `workspace.list` phase settles to `unreachable` once the ended
/// supervisor authority actually exits, which is what makes the next
/// attach spawn a fresh `--resume` rather than reusing the dead lane —
/// see `WorkspaceDestroyRes::kept`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDestroyReq {
    pub workspace_id: String,
}

/// `workspace.destroy` response. Echoes back the slug + label so the
/// frontend status line can identify what got destroyed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDestroyRes {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub tmux_killed: bool,
    pub toml_removed: bool,
    /// `None` for an ordinary destroy — the row above was actually
    /// removed (the pre-existing wire shape). `Some(detail)` names the
    /// invariant a default capsule row's request instead serves: the row
    /// is NEVER removed, so `tmux_killed`/`toml_removed` above are
    /// meaningless placeholders (`false`, nothing attempted) and this is
    /// the only field carrying what actually happened — a human-readable
    /// outcome of ending its capsule run (e.g. "ended run of 'local'",
    /// "run of 'local' was not running").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<String>,
}
