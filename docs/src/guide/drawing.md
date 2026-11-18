# Drawing and selecting for the agent

A session's agent can be given what you mark in the preview pane: a drawing over an image, a drawing on a blank
canvas, or text you select. It goes to the selected session, the one the agent pane shows, with your next prompt, so
you can circle a peak in a figure and ask what it is, or select a paragraph and ask for a shorter one. This page calls
a drawing or a selection *shared* while the agent is given it.

This works in Claude Code sessions only; see [What is not built](#What-is-not-built).

## Before you start

- **Keys.** Keys are written as on Linux and Windows; on a Mac, press Cmd where this page says Ctrl. A letter in a key
  is always lowercase: `Shift+d` is Shift and d.
- **Which pane has the keys.** The window's three columns are the navigation tree, the preview pane and the agent
  pane, and one pane at a time has the keys: its title bar shows its name and its keys. `Ctrl+ArrowRight` gives the keys
  to the pane on the right and `Ctrl+ArrowLeft` to the pane on the left, so from the navigation tree one
  `Ctrl+ArrowRight` reaches the preview and a second reaches the agent pane.
- **The drawing keys belong to the preview.** `d`, `u`, `Delete` and `Shift+d` do what this page says only while the
  preview has the keys. In the navigation tree `d` downloads the selected file, `u` uploads files, and `Shift+d`
  pressed twice on a session in Sessions mode ends that session. Look at the preview's title bar before you press
  them.
- **Words in the window's messages.** The messages quoted on this page say `row` for a session, and `daemon` and
  `host` for Ship of Tools' service (`sotd`, the background program that runs the sessions) and the computer it runs
  on.

## Drawing on an image

1. Press `f` in the navigation tree for Files mode and select an image file: PNG, JPEG, GIF, BMP, WebP or TIFF. The
   preview shows it.
2. Press `Ctrl+ArrowRight` to give the preview the keys, then `d`. A box at the foot of the preview says
   `drawing on: press and drag on the image; u undo, Delete clear, d off`.
3. Hold the left mouse button and drag over the image. A red line follows the pointer.
4. Release the button. The box says `drawing shared: the agent sees it at its next prompt`.
5. Press `Ctrl+ArrowRight` again to give the agent pane the keys, type your question and send it.
   [What the agent receives](#What-the-agent-receives) says what arrives with it.

With the preview holding the keys:

| Key or button | What it does |
|---|---|
| `d` | Turn drawing on or off over the shown image. Turning it off leaves the ink on the image, and shared. |
| Left button, held | While drawing is on: draw a stroke. Each stroke is shared when you release it. |
| Right button, held | While drawing is on: erase each stroke the pointer crosses. |
| `u`, `Ctrl+z` | While drawing is on: undo the last stroke or the last erase; right after `Delete`, bring the cleared drawing back. |
| `Delete` | While drawing is on: clear the drawing. |
| `=`, `-`, arrow keys | Zoom in, zoom out and pan, as without a drawing. The ink stays where you drew it on the image. |
| `Shift+d` | Show or hide [the canvas](#The-canvas). |

## The canvas

With the preview holding the keys, `Shift+d` covers the preview with a blank white page, whatever file it showed. The
box says `canvas: draw with the pointer; u undo, Delete clear, Shift+d back`. Drawing is always on there: the two
mouse buttons, `u`, `Ctrl+z` and `Delete` work as in the table above, and each stroke is shared when you release it.

`Shift+d` again shows the preview as it was, and so does selecting another file. While the canvas is hidden its
drawing is kept but not shared; showing the canvas again shares it again.

A session has one drawing at a time. The first stroke on another image, or on the canvas, replaces it, and the window
says `the drawing on <name> was replaced`. Each session keeps its own drawing in the window while you work in another
session. Closing the window loses every drawing.

## Selecting text

1. Show a text file of the project in the preview: markdown, source code or plain text.
2. Drag over the text with the left mouse button, and release. The status line, the line starting `status:` at the top
   of the navigation tree, says `selection shared: the agent sees it at its next prompt`.
3. Press `Ctrl+ArrowRight` to give the agent pane the keys, type your prompt and send it.

Nothing is sent while you drag. The selection is the one `Ctrl+c` copies.

Only text of a file in the project, selected in the preview, is shared. A selection in the agent pane, in a
[concept annotation](concept-layer.md) or in a rendered table is not, and nothing can be selected in an image.

In a rendered markdown preview the agent is given the lines of the markdown file that the selected text came from, as
whole blocks: each paragraph or heading the selection touches (in a list item, the item's paragraph), each code line
it touches, and any table, equation or figure between them. Link definitions (`[name]: url` lines) just above a
touched paragraph are included. A selection of nothing but a table, an equation, a figure or a rule is not shared.

The window says `selection not shared: this view is not the file's text` when the file changed after the preview read
it, and when the file, before the end of the selection, breaks a line in an unusual way: with a carriage return alone
or with one of Unicode's line separators.

## What the agent receives

When you release a selection or a stroke, the window tells Ship of Tools' service on the computer that runs the
session. At your next prompt Claude Code shows one line in the agent pane, after its own words `UserPromptSubmit
says:`, and the agent is given the rest:

| You shared | Claude Code shows | The agent is given |
|---|---|---|
| A selection | `Selection: <file> lines <a>-<b>` | The selected text, which the service reads from the file itself, labeled as your selection with the file and its line numbers. |
| A drawing on an image | `Drawing: <file> (<n> strokes)` | The image's path and the path of a picture of the image with your ink on it, with the request to read the picture to see what you marked. |
| A drawing on the canvas | `Drawing: sketch (<n> strokes)` | The path of a picture of the canvas, with the request to read it. |

Every later prompt carries it again for as long as it is shared.

- **A long selection.** The agent is given the first 2000 characters of what is shared and is told how long the whole
  was. In a markdown preview whole paragraphs are shared, so in a long paragraph the cut can fall before the words you
  selected.
- **The picture.** The service writes it to `.sot/captures/ink-<id>.png` in the project's folder, where `<id>` is the
  id Ship of Tools gave the session: one file per session, written again at each change to the drawing. It is a PNG
  copy of the image with the ink on it, scaled down to 2000 pixels on its longer side when the image is larger. The
  image file itself is never changed. The picture stays in the folder after the drawing stops being shared, until the
  session's next drawing writes over it; add `.sot/captures/` to the project's `.gitignore` if git should not see it.
- **One thing at a time.** The agent is given a selection or a drawing, never both: the newest replaces the other.
  When two windows show one session, the newest from either window is the one shared, and when either window stops
  sharing it, it is no longer shared from the other.

## What stops the sharing

A drawing or a selection is shared while it is on screen in the selected session's preview: the ink on the shown image
or canvas, the highlight on the selected text. When it leaves the screen the agent is no longer given it, and the
window shows no message for that. To be sure of what the agent has, look at the preview before you send a prompt.

- **To stop sharing a selection:** click in the preview, or press `Escape` with the preview holding the keys. The
  same `Escape` gives the keys back to the navigation tree, or, in a maximized pane, restores the layout.
- **To stop sharing a drawing:** clear it with `Delete`, or erase or undo until no ink is left. Turning drawing off
  with `d` does not stop it.

These remove the highlight of a selection, and so stop sharing it; select again afterwards:

- showing another file, or switching to another session;
- opening the editor (`e`);
- a change to the file, which refreshes its preview, or to the text size;
- in a markdown preview, the arrival of an equation, a figure or a code block's colors, which lays the document out
  again.

These hide a drawing, and so stop sharing it until its image or the canvas is shown again in that session, when it is
shared again by itself:

- showing another file, or the canvas over the image;
- opening the editor (`e`);
- switching to another session.

A selection under the canvas is kept, and is shared again when the canvas is hidden.

Two more things stop the sharing whatever is on screen. When the window loses its connection to the computer that runs
the session, nothing is shared until the connection is back, and then the window shares again what it shows. And when
the window says a new selection or drawing was not shared, the one shared before it is no longer shared either (a
refusal that says the session is not known there changes nothing).

What the agent was already given stays in that conversation's history: asked later, it can still say what it was
given. A new conversation forgets it.

## Other things the window says

A message about a drawing shows on the status line and in the box at the foot of the preview; a message about a
selection shows on the status line alone, and a `Can't draw` message in the box alone.
[Messages about the preview](previews.md#Messages-about-the-preview) says how long the box stays. The message for `d`
over a picture that is not an image file is under [What is not built](#What-is-not-built).

| The window says | What it means |
|---|---|
| `the drawing is full: undo or clear` | The drawing is at its fixed limit of ink, a limit of Ship of Tools. Undo or clear to draw more. |
| `Can't draw here: there is no image file to draw on.` | `d` where the preview shows no image file. |
| `Can't draw while the connection notice covers the preview.` | Ship of Tools' service on that computer refused this window, and the notice saying so covers the preview. |
| `selection held: this row's agent started before it could take one; restart the row to share`, `drawing held: ...` | The session's agent was started without the part of Ship of Tools that hands it a selection or a drawing. End the session (`Shift+d` twice on it in Sessions mode), start a new one in the same folder, and select or draw again; `/resume` in the new session brings the earlier conversation back. This happens to a session started before 0.7.1 was installed, when Ship of Tools' files for Claude Code (`~/.sot-comm/claude-layer`) are missing (run the installer, see [Updating](updating.md)), and to a session started with a `--settings` argument of its own that the launch could not merge. |
| `selection not shared: this row's agent takes none`, `drawing not shared: this row's agent takes none` | The session's agent is not Claude Code. |
| `selection not shared: this host's daemon does not take selections`, `drawing not shared: this daemon cannot take a drawing; update it` | The computer that runs the session has an older Ship of Tools. Update it. |
| `drawing not shared: the image is over 40 megapixels` | The image is too large to draw on. |
| `drawing not shared: the daemon cannot read this image` | The service could not read the file as a picture. |
| `selection not shared: <reason>`, `drawing not shared: <reason>` | Any other refusal, with the service's own word for it: `unknown_workspace` (the session is not known there), `bad_node_id` (the preview does not show a file of the project) or `publish_failed` (select or draw again). |

## What is not built

- **Sessions other than Claude Code.** A Codex session's agent is given neither a selection nor a drawing; the window
  says so each time, with the `takes none` message in the table above.
- **Drawing over a PDF page, an SVG or a video's poster frame.** `d` works over image files only. Over any other
  picture it draws nothing, and the box says, for a PDF page:

  `Can't draw on this PDF page. Drawing (d) marks up an image file (PNG, JPEG, TIFF and the like) for this row's agent, which sees your marks with your next prompt. To draw anyway, open a PNG of it, or press Shift+d for a blank canvas.`

  To draw on such a picture, save it as a PNG (plotting code can usually save one beside the PDF or SVG) and draw on
  that, or draw on the canvas.
