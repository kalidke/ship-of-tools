# Keybindings and contextual help

The focused pane's border shows useful actions with their **loaded shortcuts**.
Help and keyboard dispatch use the same action catalog and context checks.

- **Ctrl+?** shows the current pane's actions for five seconds, followed by a short fade.
- Press **Ctrl+? again** while it is visible to open the same context in the **Help drawer**.
- **F1** opens or closes Help directly. Both shortcuts are rebindable.
- **Escape** dismisses the temporary overlay. Any ordinary command dismisses it and still performs its normal action.
- If the action list cannot fit legibly inside the pane, Help opens in the drawer directly.

Help is a fourth view of the existing drawer, alongside Julia, Terminal and
Monitor. Switching to Help leaves Julia running. Escape returns to the previous
drawer and focus; choosing Julia, Terminal or Monitor switches to that view.
Help remembers which pane you asked about while you browse or search it.

In the Help drawer, type to search action names, descriptions or shortcuts;
use arrows to select, Tab to switch between this pane and all panes, and Enter
to open the selected action's manual section. A shadowed shortcut is identified
instead of being advertised as working. Embedded terminal applications own
their own shortcuts; the catalog describes SoT controls around them.

## Configuration

Each action maps to one chord or a list of chords in a `[keys]` table:

```toml
[keys]
help.toggle = "Ctrl+?"
drawer.help = "F1"
files.run_current = "F8"
preview.png.zoom_in = ["Shift+ArrowUp", "+", "="]
```

The first existing configuration file is selected, in this order. Its listed
actions override the defaults; unlisted actions keep their defaults:

1. `$SOT_KEYBINDINGS`.
2. `.sot/keybindings.toml`, found by walking upward from the frontend's working directory.
3. `$HOME/.config/sot/keybindings.toml`.
4. Built-in defaults.

Bindings load on frontend startup. Invalid entries log a warning and retain the
previous binding. The active Help drawer and pane borders reflect that resolved
result. When two actions share a chord in the same context, catalog order defines
dispatch precedence; Help identifies a shadowed binding.

Where a pane takes typed text (agent, terminal, Julia input, the editor,
prompts, Help's search), a binding with no `Ctrl`, `Alt` or `Super` never
fires: the character is typed. Bind such actions with a modifier if they must
work there; Help shows them as unavailable in those panes.

## Modifiers and keyboard layouts

Supported modifier names are `Ctrl`/`Control`, `Alt`/`Option`, `Shift`, and
`Super`/`Cmd`/`Command`/`Win`. The legacy `Meta` alias continues to mean `Alt`.
`Primary` means Command on macOS and Control on Windows/Linux.

The **frontend OS** determines shortcut notation, including when a Mac frontend
connects to a Linux backend: Control is `⌃`, Command is `⌘`, Option is `⌥`, and
Shift is `⇧`. Explicit Control bindings remain Control; they do not silently
become Command bindings. A Command binding must match the Command modifier.

Letter and named-key bindings match Shift explicitly: `r` and `Shift+r` are
different actions, and Shift+ArrowUp does not also pan an image. A bare uppercase
letter is shorthand for its shifted binding. Modified letter spelling is case
insensitive (`Ctrl+s` and `Ctrl+S` mean the same chord).

Character shortcuts use the active keyboard layout. `Ctrl+?` means Control plus
the question-mark character, without assuming its physical key position.
`Ctrl+Shift+/` explicitly binds the shifted slash key in the active layout.
Option-modified letters also match the layout's unmodified letter on macOS.
All shortcuts can be reassigned for layouts or OS shortcuts that conflict.

## Default actions

These are built-in defaults. In-app Help displays the bindings actually loaded.
Normal text entry and terminal-application controls retain their own input handling.

| Action | Default shortcut(s) | Description |
|---|---|---|
| `help.toggle` | `Ctrl+?` | Show actions here for five seconds; press again to browse them in the Help drawer. |
| `drawer.help` | `F1` | Browse and search actions for the pane you were using. Escape restores the previous drawer. |
| `transport.reconnect` | `F5` | Retry backend connections immediately. |
| `view.fullscreen` | `F11` | Toggle borderless fullscreen. |
| `font.scale_up` | `Ctrl+=` / `Ctrl++` | Increase the frontend font size. |
| `font.scale_down` | `Ctrl+-` / `Ctrl+_` | Decrease the frontend font size. |
| `font.scale_reset` | `Ctrl+0` | Restore the default frontend font size. |
| `focus.pane_left` | `Ctrl+ArrowLeft` | Move keyboard focus to the pane on the left. |
| `focus.pane_right` | `Ctrl+ArrowRight` | Move keyboard focus to the pane on the right. |
| `focus.pane_up` | `Ctrl+ArrowUp` | Move keyboard focus to the pane above. |
| `focus.pane_down` | `Ctrl+ArrowDown` | Move keyboard focus to the drawer or pane below. |
| `workspace.cycle_next` | `Shift+ArrowRight` | Switch to the next workspace. |
| `workspace.cycle_prev` | `Shift+ArrowLeft` | Switch to the previous workspace. |
| `pane.maximize` | `Alt+=` | Fill the window with the focused pane. |
| `pane.restore` | `Escape` | Undo maximization, then wide preview, one layer at a time. |
| `layout.wide_preview` | `Alt++` | Hide or show the agent column to give the preview more room. |
| `view.selfie` | `Ctrl+Shift+S` | Save a PNG of the whole frontend window. |
| `drawer.repl` | `Ctrl+j` | Show or hide the Julia REPL. Changing drawer views keeps Julia running. |
| `drawer.terminal` | `Ctrl+t` | Show or hide the frontend-local terminal. |
| `drawer.monitor` | `Ctrl+m` | Show or hide host resource charts. |
| `view.scroll_line_up` | `Alt+ArrowUp` | Scroll without moving the input cursor. |
| `view.scroll_line_down` | `Alt+ArrowDown` | Scroll without moving the input cursor. |
| `preview.table_left` | `h` / `ArrowLeft` | Move horizontally through a wide text table. |
| `preview.table_right` | `l` / `ArrowRight` | Move horizontally through a wide text table. |
| `preview.table_reset` | `0` | Return to the left edge of a wide table. |
| `session.create_codex` | `Ctrl+Enter` | Create a workspace in the selected folder with a Codex agent. |
| `session.create_bare` | `Shift+Enter` | Create a workspace in the selected folder with a shell and no agent. |
| `session.create` | `Enter` | Create a workspace in the selected folder with a Claude Code agent. |
| `quit` | `Ctrl+q` | Close the frontend from navigation focus. |
| `files.copy_path` | `Ctrl+c` / `c` | Copy the selected file's backend path to the clipboard. |
| `files.new` | `Ctrl+n` | Create a file in the selected directory after entering its name. |
| `files.delete` | `Ctrl+d` | Delete the selected file after confirmation. Directories are refused. |
| `nav.down` | `ArrowDown` | Select the next row. |
| `nav.up` | `ArrowUp` | Select the previous row. |
| `nav.expand` | `ArrowRight` | Expand the selected node or descend into a folder. |
| `nav.open` | `Enter` | Open the selected node; in Sessions or Hosts, select that target. |
| `nav.collapse` | `ArrowLeft` | Collapse the node or go to its parent. |
| `mode.files` | `f` | Browse project files. |
| `mode.modules` | `m` | Browse Julia modules and methods. |
| `mode.sessions` | `s` | Browse workspaces and agent sessions. |
| `files.pin` | `p` | Keep the selected file in preview while browsing. Unpin returns to the pinned file. |
| `mode.hosts` | `h` | Browse backend hosts. |
| `files.toggle_hidden` | `.` | Show or hide dotfiles. |
| `session.destroy` | `Shift+d` | Press twice to destroy the selected session. Any other command cancels confirmation. |
| `file.open` | `o` | Open HTML, video or an interactive document in the browser; Julia files open in Pluto. |
| `file.docs` | `Shift+w` | Open the built documentation site for this file in the browser. |
| `file.execute` | `Shift+o` | Render a Quarto document and execute its code chunks. |
| `files.download` | `d` | Download the selected file to the frontend machine. |
| `files.upload` | `u` | Choose local files to upload into the selected directory. |
| `files.run_fresh` | `r` | Restart Julia in this project and run the selected Julia file. Existing REPL variables are cleared. |
| `files.run_current` | `Shift+r` | Run the selected Julia file using the current Julia session and its variables. |
| `repl.clear` | `Ctrl+l` | Clear the displayed REPL output; Julia's variables remain. |
| `input.paste` | `Ctrl+v` / `Super+v` / `Shift+Insert` | Paste the clipboard into this pane. |
| `agent.copy` | `Ctrl+Shift+c` | Copy selected agent output to the clipboard. |
| `preview.page_next` | `n` / `PageDown` | Show the next page of the displayed document. |
| `preview.page_prev` | `p` / `PageUp` | Show the previous page of the displayed document. |
| `preview.png.reset` | `r` / `0` | Reset zoom and pan to fit the image in the pane. |
| `preview.png.zoom_in` | `Shift+ArrowUp` / `+` / `=` | Enlarge the displayed image. Fit image restores the overview. |
| `preview.png.zoom_out` | `Shift+ArrowDown` / `-` | Reduce image zoom down to fit-to-pane. |
| `preview.png.pan_left` | `ArrowLeft` | Move across the zoomed image. |
| `preview.png.pan_right` | `ArrowRight` | Move across the zoomed image. |
| `preview.png.pan_up` | `ArrowUp` | Move across the zoomed image. |
| `preview.png.pan_down` | `ArrowDown` | Move across the zoomed image. |
| `preview.scalebar.toggle` | `Ctrl+s` | Toggle the physical scalebar, or enter the pixel size when scale is unknown. |
| `view.return_nav` | `Escape` | Move focus back to the navigation tree. |
| `preview.capture` | `c` / `Ctrl+c` | Crop the visible image region and attach it to the agent input. |
| `preview.copy_code` | `y` | Copy the displayed markdown code blocks to the clipboard. |
| `preview.edit` | `e` | Edit the displayed file or its concept annotation. |
| `view.page_up` | `PageUp` | Scroll back through this pane. |
| `view.page_down` | `PageDown` | Scroll forward through this pane. |
| `preview.up` | `ArrowUp` | Move up through the displayed text. |
| `preview.down` | `ArrowDown` | Move down through the displayed text. |
| `preview.start` | `Home` | Scroll to the beginning of the displayed text. |
| `preview.end` | `End` | Scroll to the end of the displayed text. |
| `preview.half_up` | `Ctrl+u` | Scroll up half a pane. |
| `preview.half_down` | `Ctrl+d` | Scroll down half a pane. |
| `help.up` | `ArrowUp` | Previous action in the Help drawer. |
| `help.down` | `ArrowDown` | Next action in the Help drawer. |
| `help.page_up` | `PageUp` | Previous actions in the Help drawer. |
| `help.page_down` | `PageDown` | Next actions in the Help drawer. |
| `help.scope` | `Tab` | This pane or all panes in the Help drawer. |
| `help.manual` | `Enter` | Open manual in the Help drawer. |
| `help.close` | `Escape` | Return to work in the Help drawer. |
| `files.confirm_delete` | `y` / `Shift+y` | Delete the file named in the confirmation prompt. Any other key cancels. |
| `edit.confirm_discard` | `y` / `Shift+y` / `Escape` | Discard unsaved edits and close the editor. Another key returns to editing. |
| `edit.reload` | `r` / `Shift+r` | Discard this edit buffer and reload the externally changed file. |
| `edit.keep` | `k` / `Shift+k` | Dismiss the external-change warning and continue editing. |
| `edit.undo` | `Ctrl+z` / `Super+z` | Undo in the editor. |
| `edit.redo` | `Ctrl+y` / `Super+Shift+z` | Redo in the editor. |
| `edit.copy` | `Ctrl+c` / `Super+c` | Copy selection in the editor. |
| `edit.cut` | `Ctrl+x` / `Super+x` | Cut selection in the editor. |
| `edit.paste` | `Ctrl+v` / `Super+v` / `Shift+Insert` | Paste in the editor. |
| `edit.newline` | `Enter` | Insert newline in the editor. |
| `edit.backspace` | `Backspace` | Delete previous character in the editor. |
| `edit.delete` | `Delete` | Delete next character in the editor. |
| `edit.left` | `ArrowLeft` / `Shift+ArrowLeft` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.right` | `ArrowRight` / `Shift+ArrowRight` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.up` | `ArrowUp` / `Shift+ArrowUp` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.down` | `ArrowDown` / `Shift+ArrowDown` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.home` | `Home` / `Shift+Home` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.end` | `End` / `Shift+End` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.page_up` | `PageUp` / `Shift+PageUp` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.page_down` | `PageDown` / `Shift+PageDown` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.start` | `Ctrl+Home` / `Shift+Ctrl+Home` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.finish` | `Ctrl+End` / `Shift+Ctrl+End` | Move the editor cursor. Hold Shift to extend the selection. |
| `edit.save` | `Ctrl+s` / `Super+s` | Save the current file or concept annotation. |
| `input.cancel` | `Escape` | Cancel the current input. Unsaved edits require confirmation. |
| `input.confirm` | `Enter` | Submit the entered filename or value. |
| `repl.interrupt` | `Ctrl+c` | Interrupt the current evaluation, or clear the input when Julia is idle. |
| `repl.history_prev` | `ArrowUp` | Recall the previous Julia input. |
| `repl.history_next` | `ArrowDown` | Recall the next Julia input. |
| `picker.parent` | `Backspace` | Go to the parent folder in the new-session picker. |
| `repl.submit` | `Enter` | Evaluate the current Julia input. |
| `repl.newline` | `Shift+Enter` | Continue the Julia input on another line. |
