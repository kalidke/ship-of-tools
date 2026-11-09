Part of fe-ui: see ../CLAUDE.md.

Mouse routing focuses the agent pane or flowed text preview before starting its selection. A left press outside
each pane clears that owner's selection; motion extends an active drag to the visible edge and release ends it.
The preview uses ShapedPreview's buffer positions and rect, and is selectable only while flowed text is shown.

The drawer's tenant decides the pane a key meets: `help::Pane::of_drawer` is the one exhaustive map from `DrawerContent`
to `Pane`, and help (`Context::allows`), the wheel, the line-scroll chords (`line_scroll_target`) and `drawer_pane_key`
all read it. Only the Repl pane reaches the REPL's draft, paste, scrollback and history; Chat, Monitor and Help take
no text and scroll nothing, and Escape returns to the nav. Chat is a tenant (`drawer.chat`, `Primary+Shift+k`) that shows nothing yet.
Pinned by `help_tests::the_chat_drawer_is_a_tenant_whose_keys_reach_no_repl_handler`.
