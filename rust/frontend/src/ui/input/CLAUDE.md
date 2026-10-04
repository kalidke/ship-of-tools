# rust/frontend/src/ui/input: key bindings, help and paste (fe-ui)

One action catalog serves key dispatch, button labels and contextual help, so the three never disagree. The Help drawer
and the clipboard paste helpers live beside it. Part of the window; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: the folder's module declarations.
- `keybindings.rs`: the action catalog (`ACTIONS`), chords, defaults and `keybindings.toml` merging.
- `keybindings_tests.rs`: tests of the catalog, chord parsing, merging, literal-text and shifted punctuation.
- `help.rs`: the contextual help model (`Context`) and its rows, with its tests inline.
- `help_drawer.rs`: `State`'s help drawer: `help_context`, `open_help_drawer`, `close_help_drawer`, `help_peek_expired`.
- `paste.rs`: `read_clipboard_text`, `bracketed_paste_bytes` and the two forwarders to the agent pane and the Terminal drawer.
- `mouse.rs`: Pointer events: cursor moves, clicks and the wheel, each sent to the pane it acts on.
- `keypress.rs`: One keypress, from the key event to the layer that takes it.

## Start here
`ACTIONS` and `KeyBindings::resolve` in keybindings.rs for a new action or chord; `help::Context` in help.rs for what
the help view shows in each pane.

## Rules
- One catalog serves dispatch, labels and help (`ACTIONS`); `KeyBindings::resolve` takes the `allowed` predicate that
  `help::Context::allows` supplies.
- Help is `Primary+?` (`help.toggle`) and `F1` (`drawer.help`); there is no bare `?` chord.
- A `keybindings.toml` entry replaces that action's default chords (`KeyBindings::merge_text`).
- A bare-character override never fires where the pane types text (`KeyBindings::resolve`'s `literal_text`).
- The call order in `route_key` is the keys' precedence: quit prompt, help, the shared keys, then the
  focused pane. A layer that takes a key returns `Break`, which skips every later layer and the keypress tail.
