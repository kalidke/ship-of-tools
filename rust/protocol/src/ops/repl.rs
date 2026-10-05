// the persistent Julia REPL: eval, run_file, execute and its streamed frames

use super::*;

/// Submit a chunk of Julia code to the persistent REPL. Phase-1 is
/// synchronous-collect: the response carries the full `frames` list
/// (stdout, stderr, value, error, done) for this eval. Streamed evt
/// delivery is a phase-2 enhancement; the payload shape doesn't change.
///
/// `mode` is optional ("julia" | "pkg", default "julia") — `"pkg"`
/// routes the line through `Pkg.REPLMode.do_cmds` for `pkg>`-style
/// commands (`b07b4f0`). Omitted on the wire when None so the
/// envelope stays compatible with older backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplEvalReq {
    pub eval_id: u64,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// ADR 0014 workspace routing. The backend resolves this to the
    /// per-workspace `Repl` handle so `x = 5` in workspace A doesn't
    /// leak into workspace B. Missing = default workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplEvalRes {
    pub eval_id: u64,
    pub elapsed_ms: u64,
    /// Phase-2 streaming: frames now arrive as `repl.frame` evts, so this
    /// is empty on the response (terminal ack). `#[serde(default)]` keeps
    /// the field optional on the wire for forward/backward compatibility.
    #[serde(default)]
    pub frames: Vec<ReplFrame>,
}

/// Run a `.jl` file. `fresh:true` RESTARTS the persistent REPL into `path`'s
/// project: the daemon bounces the julia child (`Repl::restart_with_project`
/// spawns a fresh `julia --project=<project>` supervisor) and rewrites the
/// request to a plain `include` in that fresh process, so the Julia side never
/// sees `fresh:true`. This is the way to clear a stale kernel after a change
/// `include` alone can't apply (e.g. a struct/field redefinition). `fresh:false`
/// calls `include(path)` inside the *current* persistent REPL with no reset.
/// Either way the include streams the same frame shapes as `repl.eval`. Project
/// is auto-detected by walking up from `path` for a `Project.toml`; the
/// persistent REPL's active project is the fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplRunFileReq {
    pub eval_id: u64,
    pub path: String,
    #[serde(default)]
    pub fresh: bool,
    /// ADR 0014 workspace routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplRunFileRes {
    pub eval_id: u64,
    pub path: String,
    pub fresh: bool,
    pub elapsed_ms: u64,
    /// Directory passed as `--project=` (when fresh:true) or — for
    /// fresh:false — the project the REPL believes the file belongs to,
    /// surfaced so the frontend can warn on mismatches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    /// "discovered" (a Project.toml was found), "fallback" (REPL's
    /// active project), or "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_source: Option<String>,
    /// Phase-2 streaming: see `ReplEvalRes::frames`. Empty on the response;
    /// frames stream as `repl.frame` evts.
    #[serde(default)]
    pub frames: Vec<ReplFrame>,
}

/// Serde default for `ReplFrame::Browser::open` — frames from shims that
/// predate the field keep the original auto-open behavior.
fn default_open() -> bool {
    true
}

/// Per ADR 0009. `kind`-tagged enum on the wire so new frame kinds (image
/// blobs, html, …) land additively.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReplFrame {
    /// Control frame (ADR 0033 phase 2): a run STARTED. Emitted by the BACKEND
    /// (not the shim) before a `repl.execute` submission so a front-end can
    /// pre-register a drawer entry in submission order — even for a silent or
    /// long eval, and even when the run belongs to another client. The output
    /// frames + terminal `done` for this `eval_id` follow. Front-ends that
    /// don't recognise it ignore it (it carries no output).
    Started {
        run_id: String,
        /// Who initiated the run (e.g. "session", a sot-comm handle).
        origin: String,
        /// Human display of what's running (file basename or a code preview).
        display: String,
    },
    Stdout {
        text: String,
    },
    Stderr {
        text: String,
    },
    Value {
        mime: String,
        text: String,
    },
    /// Result is a showable binary (CairoMakie figure, raster Plot, …).
    /// The REPL prefers this over `value` when `showable(MIME"image/...",
    /// result)` is true so the frontend can rasterise without parsing
    /// `text/plain`. Bytes are inline base64 — same convention as
    /// `file.preview`'s `blob_base64` — to keep the NDJSON envelope JSON-
    /// safe without growing a sidecar-blob path through the REPL bridge.
    Image {
        mime: String,
        data_base64: String,
        bytes: u64,
    },
    /// The eval produced a live, browser-served artifact (an interactive
    /// WGLMakie/Bonito figure, a served dashboard, …) rather than a static
    /// value. `url` is loopback-shaped (`http://127.0.0.1:<port>/…`) so it
    /// resolves directly on a local FE and through the launcher's existing
    /// `-L <port>:127.0.0.1:<port>` tunnel on a remote FE — the same
    /// convention as `pluto.open` / `video.open` / `docs.open`. The frontend
    /// hands it to the OS browser-open; no bytes cross the protocol. ADR 0032.
    Browser {
        url: String,
        /// Whether front-ends should AUTO-OPEN the URL in the OS browser.
        /// `wglshow(fig; open=false)` sets this false to serve without
        /// opening anywhere — the frame still flows (so the daemon's
        /// ADR-0035 proxy allowlist still learns the port) (`fe` names the
        /// one frontend that opens it anyway). Absent (older shims)
        /// defaults to true — the original broadcast-open behavior.
        #[serde(default = "default_open")]
        open: bool,
        /// The one frontend that opens the page although `open` is false: its address (`fe@<name>`, what
        /// `sot-fe --fe <name>` targets), matched exactly as a directed `fe.command` is. Sent only with
        /// `open: false`, so a frontend that predates it opens nothing. `wglshow(fig; open = "<name>")`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fe: Option<String>,
    },
    Error {
        message: String,
        stacktrace: Vec<StackFrame>,
    },
    Done {
        eval_id: u64,
        elapsed_ms: u64,
    },
    /// Control frame: the workspace's persistent REPL child changed lifecycle
    /// state. Emitted by the BACKEND supervisor (not the shim — like
    /// `Started`) so a front-end can tell a *starting* REPL from a dead or
    /// silent one: the first Julia child in a workspace precompiles its
    /// project env (per-package REPL env, #44), which can take minutes with
    /// zero output frames — indistinguishable from a dead kernel without
    /// this. `state` is one of "starting" (child spawned, `using
    /// ShipToolsRepl` still compiling) | "ready" (serve loop up — the child's
    /// first stdout line) | "dead" (child exited; it respawns on the next
    /// eval). Not eval routing: the evt's `eval_id` is 0 and front-ends key
    /// this off the evt's `workspace_id`. Old front-ends fail this one evt's
    /// parse and drop it (warn), which is the additive contract.
    Lifecycle {
        state: String,
    },
}

/// Payload of a `repl.frame` evt (ADR 0009 phase-2 streaming). One frame,
/// pushed as it is produced. `eval_id` correlates the frame to the originating
/// `repl.eval` / `repl.run_file` request; `workspace_id` lets a frontend that
/// has swapped workspaces (ADR 0014) route the frame to the right pane even
/// after the active workspace changed. The inner `frame` is the same tagged
/// `ReplFrame` that previously rode the response, unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplFrameEvt {
    pub eval_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub frame: ReplFrame,
}

/// Request for `repl.execute` (ADR 0033): run something in a workspace's
/// persistent REPL and collect the output into the response. `workspace_id`
/// is required (unlike the eval/run_file ops it is not optional — an external
/// caller must name the target REPL explicitly). `timeout_ms` bounds the wait;
/// on timeout the run is NOT interrupted (it keeps running, its frames still
/// reach the drawer), the response just returns `outcome:"timeout"` with
/// whatever was collected so far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplExecuteReq {
    pub workspace_id: String,
    pub input: ReplExecuteInput,
    /// Wall-clock budget in ms. Default 120_000, clamped to [1_000, 1_800_000].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Who initiated the run, surfaced in the user's drawer when the run is
    /// shown as a shared entry (ADR 0033 phase 2). Defaults to "session".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// What to run. `run_file` resolves `path` against the workspace root, requires
/// an existing regular `.jl` file, and `include`s it in the persistent REPL
/// (`fresh:false` semantics — no reset). `eval` runs a code chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplExecuteInput {
    RunFile {
        path: String,
    },
    Eval {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
}

/// A `value` frame surfaced in the collected response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplValueOut {
    pub mime: String,
    pub text: String,
}

/// The terminal error frame surfaced in the collected response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplErrorOut {
    pub message: String,
    #[serde(default)]
    pub stacktrace: Vec<StackFrame>,
}

/// Response for `repl.execute`. `outcome` is the authoritative terminal state,
/// derived from the shim's terminal `res` plus the collected frames — one of
/// `ok | error | busy | interrupted | timeout | repl_died`. Text is bounded
/// (`truncated` set if a flood was clipped); figures are written to files and
/// returned as paths (base64 is NOT inlined — it would blow the 1 MiB cap).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplExecuteRes {
    pub run_id: String,
    pub workspace_id: String,
    pub outcome: String,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub values: Vec<ReplValueOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReplErrorOut>,
    /// Absolute paths to figure images spilled under `<ws>/.sot/runs/<run_id>/`.
    #[serde(default)]
    pub figures: Vec<String>,
    pub truncated: bool,
    /// For run_file: the project the REPL believes the file belongs to (the
    /// shim's mismatch warning rides `stderr`). None for eval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub file: String,
    pub line: i64,
    #[serde(rename = "fn")]
    pub function: String,
}

#[cfg(test)]
mod repl_lifecycle_tests {
    use super::{ReplFrame, WorkspaceListEntry};

    #[test]
    fn lifecycle_frame_wire_shape_is_kind_tagged() {
        // The daemon supervisor fabricates these; the FE routes on
        // kind == "lifecycle". Pin the exact wire shape.
        let f = ReplFrame::Lifecycle {
            state: "starting".into(),
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "lifecycle", "state": "starting" })
        );
        let back: ReplFrame = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ReplFrame::Lifecycle { state } if state == "starting"));
    }

    #[test]
    fn unknown_frame_kind_fails_only_that_parse() {
        // The additive contract an OLD front-end relies on: a frame kind it
        // doesn't know fails ITS deserialization (the transport warn-drops
        // that one evt) — it must not silently mis-parse into another
        // variant. This is the same property `lifecycle` rode in on.
        let json = serde_json::json!({ "kind": "from_the_future", "x": 1 });
        assert!(serde_json::from_value::<ReplFrame>(json).is_err());
    }

    #[test]
    fn workspace_list_entry_without_repl_state_defaults_empty() {
        // Mid-rollout tolerance: a daemon that predates `repl_state` (and the
        // other #[serde(default)] fields) still deserializes; the FE reads ""
        // and keeps any frame-driven lifecycle entry instead of regressing.
        let json = serde_json::json!({
            "workspace_id": "ws-a-123",
            "slug": "a",
            "label": "A",
            "project_root": "/p",
            "session_name": "t",
            "kernel_running": false,
            "is_default": false,
        });
        let e: WorkspaceListEntry = serde_json::from_value(json).unwrap();
        assert_eq!(e.repl_state, "");
    }

    #[test]
    fn workspace_list_entry_repl_state_round_trips() {
        let json = serde_json::json!({
            "workspace_id": "ws-a-123",
            "slug": "a",
            "label": "A",
            "project_root": "/p",
            "session_name": "t",
            "kernel_running": false,
            "is_default": false,
            "repl_state": "starting",
        });
        let e: WorkspaceListEntry = serde_json::from_value(json).unwrap();
        assert_eq!(e.repl_state, "starting");
    }
}

#[cfg(test)]
mod browser_frame_tests {
    use super::*;

    #[test]
    fn a_browser_frame_names_its_one_frontend_only_when_aimed() {
        let v = serde_json::json!({"kind": "browser", "url": "http://127.0.0.1:1/x"});
        let f: ReplFrame = serde_json::from_value(v).unwrap();
        match &f {
            ReplFrame::Browser { open, fe, .. } => {
                assert!(*open);
                assert_eq!(*fe, None);
            }
            other => panic!("not a browser frame: {other:?}"),
        }
        let back = serde_json::to_value(&f).unwrap();
        assert!(back.get("fe").is_none(), "{back}");

        let v = serde_json::json!({"kind": "browser", "url": "http://127.0.0.1:1/x", "open": false, "fe": "fe@a"});
        match serde_json::from_value::<ReplFrame>(v).unwrap() {
            ReplFrame::Browser { open, fe, .. } => {
                assert!(!open);
                assert_eq!(fe.as_deref(), Some("fe@a"));
            }
            other => panic!("not a browser frame: {other:?}"),
        }
    }
}
