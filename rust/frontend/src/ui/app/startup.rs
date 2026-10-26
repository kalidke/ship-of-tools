//! The harness's startup script: the one-shots a capture or demo run asks for on the command line, each consumed
//! once its moment comes.

/// Each one-shot fires at most once: the auto-expand, auto-pin and demo eval once the first tree has landed with the
/// startup selection, the demo methods once their row appears, and the start-path walk on arrival or a dead end.
pub(in crate::ui) struct StartupScript {
    /// One-shot from `--auto-expand`; consumed after `pending_initial_selection`
    /// lands. Fires the same outgoing request the Enter/Right key would,
    /// so capture tests can verify expanded states.
    auto_expand: bool,
    /// One-shot from `--auto-pin`. Same trigger conditions as
    /// `auto_expand`: fires after the initial cursor selection
    /// has landed and the tree isn't empty. Lets `--capture` exercise
    /// the C2 pin sigil and `[pinned *]` preview-title chrome.
    auto_pin: bool,
    /// `--demo-function-methods <module>:<name>` one-shot. When the row
    /// `modules:<module>:<name>` appears in the tree (after a prior col-2
    /// expansion has landed), the chrome moves the cursor onto it and
    /// fires `function.methods`. Consumed once it triggers.
    demo_function_methods: Option<(String, String)>,
    /// `--demo-repl-eval <code>` one-shot: submit through the FE's own
    /// path once the initial tree has landed, then open the REPL drawer.
    /// Externally-dispatched evals are dropped by design (frames only
    /// render for self-created `repl_log` entries), so the harness enters
    /// by the front door.
    demo_repl_eval: Option<String>,
    /// `--start-path <relpath>` walk state (files-mode). While pending, each
    /// tree update either lands the cursor on the target file's row (done)
    /// or expands the deepest existing collapsed ancestor directory and
    /// waits for its children splice. Consumed on arrival or dead end.
    start_path: Option<String>,
    /// Ancestor row id we last fired `tree.children` for during the
    /// `--start-path` walk. Expansion flips `row.expanded` only when the
    /// reply lands, so without this memo the walk would re-fire the same
    /// request on every redraw in between.
    start_path_fired: Option<String>,
}

impl StartupScript {
    pub(in crate::ui) fn new(
        auto_expand: bool, auto_pin: bool, demo_function_methods: Option<(String, String)>,
        demo_repl_eval: Option<String>, start_path: Option<String>,
    ) -> Self {
        Self { auto_expand, auto_pin, demo_function_methods, demo_repl_eval, start_path, start_path_fired: None }
    }

    /// `--auto-expand`, once `ready` (the startup selection landed on a non-empty tree). Taken whether or not the
    /// expansion queues: a no-op row does not deserve a retry loop.
    pub(in crate::ui) fn take_auto_expand(&mut self, ready: bool) -> bool {
        let fire = self.auto_expand && ready;
        if fire {
            self.auto_expand = false;
        }
        fire
    }

    /// `--auto-pin`, under the same gate and the same single fire.
    pub(in crate::ui) fn take_auto_pin(&mut self, ready: bool) -> bool {
        let fire = self.auto_pin && ready;
        if fire {
            self.auto_pin = false;
        }
        fire
    }

    /// `--demo-repl-eval`'s code, once `ready`.
    pub(in crate::ui) fn take_demo_repl_eval(&mut self, ready: bool) -> Option<String> {
        if self.demo_repl_eval.is_some() && ready { self.demo_repl_eval.take() } else { None }
    }

    /// The row `--demo-function-methods` waits for, while it waits.
    pub(in crate::ui) fn demo_function_target(&self) -> Option<String> {
        self.demo_function_methods.as_ref().map(|(module, name)| format!("modules:{module}:{name}"))
    }

    pub(in crate::ui) fn finish_demo_function(&mut self) { self.demo_function_methods = None; }

    /// The `--start-path` target while the walk is pending.
    pub(in crate::ui) fn start_path(&self) -> Option<&str> { self.start_path.as_deref() }

    /// The frontier ancestor the walk last asked to expand.
    pub(in crate::ui) fn start_path_fired(&self) -> Option<&str> { self.start_path_fired.as_deref() }

    pub(in crate::ui) fn note_frontier_fired(&mut self, ancestor: String) { self.start_path_fired = Some(ancestor); }

    /// The walk arrived, or reached a dead end: it ends with its memo.
    pub(in crate::ui) fn finish_start_path(&mut self) {
        self.start_path = None;
        self.start_path_fired = None;
    }
}
