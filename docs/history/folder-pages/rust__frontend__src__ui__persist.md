# rust/frontend/src/ui/persist: the window's settings, config discovery and resume state (fe-ui)

Config is layered TOML read once at start; resume state is one per-host snapshot written when the user changes mode or
session. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: `State::persist_resume_state`, the one writer of the resume snapshot.
- `settings.rs`: `Settings` and its layered TOML reader (layout presets, display, download directory).
- `settings_tests.rs`: the tests of `settings.rs`.
- `discover.rs`: `find_config_file`, the one config-file finder.
- `resume.rs`: `GlobalState` and its `load`/`save`, the per-host resume file.

## Start here
`settings.rs` `Settings::load_layered` for a new setting. `resume.rs` `load`/`save` and `mod.rs`
`State::persist_resume_state` for what a launch resumes.

## Rules
- Reads fail soft to defaults: `Settings::load_layered` warns and keeps the defaults; `resume::load` returns
  `GlobalState::default()` on a missing or unreadable file and skips lines it cannot parse.
- A harness window (`--capture`, `--ephemeral`) never writes resume state, and a minimized window saves nothing:
  `State::persist_resume_state` returns early on `ephemeral` and on `is_minimized`.
- Discovery order: the `$SOT_SETTINGS` / `$SOT_KEYBINDINGS` override, then `.sot/` in the cwd's ancestors, then
  `$HOME/.config/sot/`: `find_config_file`, called with `SOT_SETTINGS`/`settings.toml` and `SOT_KEYBINDINGS`/`keybindings.toml`.
- `[display] fullscreen_vsync_pin` defaults false and is a per-box choice; never add an always-redraw path for every
  panel: `Settings`'s `Default` (test `fullscreen_vsync_pin_defaults_false_and_parses`).
- The settings merger treats '#' as a comment delimiter only outside single/double quotes; quoted values keep it (strip_comment).
