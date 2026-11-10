Part of fe-ui: see ../CLAUDE.md.

Mouse routing focuses the agent pane or flowed text preview before starting its selection. A left press outside
each pane clears that owner's selection; motion extends an active drag to the visible edge and release ends it.
The preview uses ShapedPreview's buffer positions and rect, and is selectable only while flowed text is shown.

Whether a chord that resolved to no action types its character is `chord_types_text` (`input/mod.rs`), the window's one
rule: AltGr (Ctrl with Alt) types, a Ctrl chord and a macOS Command chord do not. The editor and the chat draft call it; the REPL's draft
keeps its own narrower rule (docs/defects.md: REPL-CTRL-CHORD-TYPES). Pinned by `typing_tests`.

The drawer's tenant decides who its input reaches. `help::Pane::of_drawer` is the one exhaustive map from `DrawerContent`
to the pane the help context and the dispatch scopes see, and `help::DrawerInput::of` is the one exhaustive decision
(`Repl`, `Terminal`, `Chat`, `Inert`) that `drawer_pane_key`, the wheel and the line-scroll chords (`line_scroll_target`) match;
the first two match it with no wildcard, and `line_scroll_target` keeps two wildcard arms for the focus (Preview, and the rest, which sends a Repl focus on a Terminal or Inert tenant to nowhere). Only `Repl` (the REPL tenant, and a closed drawer that kept focus) reaches the REPL's draft, paste,
scrollback and history. `Chat` takes its reading keys (PageUp, PageDown, End, Alt+Up and Alt+Down, the wheel), `ReturnNav`, and typing, Backspace, Shift+Enter and paste into its draft; an `Inert` tenant (Monitor, Help) takes nothing but `ReturnNav`, which
Monitor does not resolve (Escape does nothing there), and Help's own keys (search, list
scroll, Escape closing it) are handled before this router. Chat is a tenant (`drawer.chat`, `Primary+Shift+k`) that shows the held
records. Pinned by `help_tests::the_chat_drawer_is_a_tenant_whose_keys_reach_no_repl_handler`, which covers both maps
and `line_scroll_target` over all six drawer states; the handlers' matches are exhaustive over `DrawerInput`, but no
test drives the handlers' arms or their call sites: an `Inert` arm that lost its `return`, or a call site handed a
constant, would pass the suite (docs/defects.md: B5A-ARMS-UNDRIVEN).
