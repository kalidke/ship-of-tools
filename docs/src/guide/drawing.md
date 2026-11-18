# Drawing and selecting for the agent

A session's agent can be shown what you mark in the preview pane: ink drawn over an image, a sketch on a blank canvas,
or text you select. The agent is given it with your next prompt, so you can circle a peak in a figure and ask what it
is, or select a paragraph and ask for a shorter one.

This works in Claude Code sessions. It is not built for Codex sessions; see [What is not built](#What-is-not-built).

Keys are written as on Linux and Windows; on a Mac, press Cmd where this page says Ctrl. A letter in a key is always
lowercase: `Shift+d` is Shift and d. The preview pane is the window's middle column. It shows the file selected in
Files mode (press `f` in the navigation tree, the left column), and `Ctrl+Right` from the tree gives it the keys.

## Drawing on an image

1. In Files mode, select an image file (PNG, JPEG, TIFF and the like). The preview shows it.
2. Press `Ctrl+Right`, then `d`. A box at the foot of the preview says
   `drawing on: press and drag on the image; u undo, Delete clear, d off`.
3. Hold the left mouse button and drag over the image. The line is red, and it stays where you drew it as you zoom
   and pan.
4. Release the button. The box says `drawing shared: the agent sees it at its next prompt`.
5. Type your question in the agent pane and send it. [What the agent receives](#What-the-agent-receives) says what
   arrives with it.

Each further stroke is shared when you release it. `d` again turns drawing off; the ink stays on the image and stays
shared.

| Key or button | What it does |
|---|---|
| `d` | Turn drawing on or off over the shown image. |
| Left button, held | Draw a stroke. |
| Right button, held | Erase each stroke the pointer crosses. |
| `u`, `Ctrl+z` | Take back the last stroke or erase; right after `Delete`, bring the drawing back. |
| `Delete` | Clear the drawing. |
| `Shift+d` | Show or hide [the canvas](#The-canvas). |

These keys are the preview's. In the navigation tree the same letters do other things (`d` there downloads the
selected file), so give the preview the keys first.

Only an image file can be drawn on. Over a PDF page, an SVG, a video's poster frame or any other picture, `d` draws
nothing and the box says why, with the two ways to draw anyway:

`Can't draw on this PDF page. Drawing (d) marks up an image file (PNG, JPEG, TIFF and the like) for this row's agent, which sees your marks with your next prompt. To draw anyway, open a PNG of it, or press Shift+d for a blank canvas.`

In the window's messages, `row` means a session.

## The canvas

With the preview focused, `Shift+d` covers the preview with a blank white page, whatever file it showed. The box says
`canvas: draw with the pointer; u undo, Delete clear, Shift+d back`. Drawing is always on there: drag with the left
button to draw, and erase, undo and clear as on an image. Each stroke is shared with the agent when you release it.

`Shift+d` again shows the preview as it was, and so does selecting another file. While the canvas is hidden its sketch
is kept but not shared; showing the canvas again shares it again.

A session has one drawing at a time. The first stroke on another image, or on the canvas, replaces it, and the window
says `the drawing on <name> was replaced`. Each session keeps its own drawing while you work in another one. Closing
the window loses every drawing.

## Selecting text

1. Show a text file of the project in the preview: markdown, source code or plain text.
2. Drag over the text with the left mouse button, and release. The status line, the line starting `status:` at the top
   of the navigation tree, says `selection shared: the agent sees it at its next prompt`.
3. Type your prompt in the agent pane and send it.

Nothing is sent while you drag. The selection is the one `Ctrl+c` copies.

Only the preview's text of a file in the project is shared. The agent pane, an image, the concept pane and a rendered
table are not. A selection is refused if the preview does not show the file's own text: a preview older than the file,
and files with unusual line breaks. A change to the file refreshes its preview, which takes the selection back; select
again to share the new text.

In a rendered markdown preview the agent is given the file's own lines that your selected text was rendered from,
whole: each paragraph (in a list item, the item's paragraph) or heading you touch and each code line, with any table,
equation or figure between them. A table, equation, figure or rule alone is not shared. A paragraph that follows link
reference definitions shares their lines too, and a paragraph written on one long line is cut at 2000 characters, which
may fall before the words you selected. The selection is refused when the file no longer holds what the preview showed,
or when a lone carriage return, U+0085, U+2028 or U+2029 comes before its end.

## What the agent receives

When you release a selection or a stroke, the window tells Ship of Tools' service on the computer that runs the
session. For a selection, the service reads the file itself and keeps the text you selected. For a drawing, it writes
one picture, the image with your ink on it or the sketch, to `.sot/captures/ink-<row id>.png` in the project; each
change to the drawing writes that file again.

At your next prompt Claude Code shows a one-line label in the agent pane, and the agent is given the rest:

| You shared | Claude Code shows | The agent is given |
|---|---|---|
| A selection | `UserPromptSubmit says: Selection: <file> lines a-b` | The text from the file, labeled with the file and its line numbers as your selection. A selection longer than 2000 characters is cut there, and the agent is told how long it was. |
| A drawing on an image | `UserPromptSubmit says: Drawing: <file> (2 strokes)` | The image's path and the picture's path, with the request to read the picture to see what you marked. |
| A sketch on the canvas | `UserPromptSubmit says: Drawing: sketch (2 strokes)` | The picture's path, with the request to read it. |

Every later prompt carries it again until you take it back.

The agent is shown one thing at a time: a selection or a drawing. The newest replaces the other. When two windows show
one session, the newest from either window is the one shared, and taking it back in either window takes it back for
both.

## Taking it back

- **A selection:** click in the preview, or press `Esc` there.
- **A drawing:** clear it with `Delete`, or erase or undo until no ink is left. Turning drawing off with `d` does not
  take it back.

Some things you do take it back as well:

- Showing another file or the canvas, opening the editor, switching to another session (the session you left is
  cleared) or closing the window. Showing the image or the canvas again shares the drawing again, and hiding the
  canvas shares the selection again.
- For a selection, a change to the file or to the text size. In a markdown preview the arrival of an equation, a
  figure or a code block's colors lays the document out again and takes the selection back too. A drawing stays on its
  image through these.
- A connection that ends (a lost link, a closed window). A window that reconnects shares its current selection or
  drawing again.
- A newer selection or drawing that the session's agent side refuses: the one before it is taken back too. A refusal
  before the session is found (`unknown_workspace`, no state folder) takes nothing back.

Taking back is not announced. What the agent was already given stays in that conversation's history: asked later, it
can still say what it was given. A new conversation forgets it.

## What the window says

A message about a drawing shows on the status line and in the box at the foot of the preview; a message about a
selection shows on the status line alone, and the refusal of `d` in the box alone.
[Messages about the preview](previews.md#Messages-about-the-preview) says how long the box stays.

| The window says | What it means |
|---|---|
| `drawing on: press and drag on the image; u undo, Delete clear, d off`, `drawing off` | `d` turned drawing on or off over the shown image. |
| `canvas: draw with the pointer; u undo, Delete clear, Shift+d back`, `canvas hidden` | `Shift+d` showed or hid the canvas. |
| `drawing cleared: u brings it back` | `Delete` cleared the ink. |
| `the drawing on <name> was replaced` | A stroke on another image, or on the canvas, replaced the session's drawing. |
| `the drawing is full: undo or clear` | The drawing holds as much ink as it can. |
| `Can't draw on this SVG. ...` (or `this PDF page`, `this video frame`, `this picture`) | `d` over a picture that is not an image file. Open a PNG of it, or use the canvas. |
| `Can't draw here: there is no image file to draw on.` | `d` where the preview shows no image file. |
| `Can't draw while the connection notice covers the preview.` | The backend refused this window, and the notice saying so covers the preview. |
| `selection shared: the agent sees it at its next prompt`, `drawing shared: ...` | The agent is given it with your next prompt. |
| `selection held: this row's agent started before it could take one; restart the row to share`, `drawing held: ...` | The agent started without the hook that hands it a selection: the session started before the release that shares selections was installed (end it, start a new one and select or draw again), the Claude Code integration under `~/.sot-comm/claude-layer` is not installed (install it first), or you pass your own `--settings` that the launch cannot merge. |
| `selection not shared: this view is not the file's text` | The preview does not show the file's own text. |
| `selection not shared: this row's agent takes none`, `drawing not shared: this row's agent takes none` | The session's agent is not Claude Code. |
| `selection not shared: this host's daemon does not take selections`, `drawing not shared: this daemon cannot take a drawing; update it` | The computer that runs the session has an older Ship of Tools. Update it. |
| `drawing not shared: the image is over 40 megapixels` | The image is too large to draw on. |
| `drawing not shared: the daemon cannot read this image` | The image file could not be read as a picture. |
| `selection not shared: <reason>`, `drawing not shared: <reason>` | Any other refusal, such as `unknown_workspace` (the session is no longer known there), `bad_node_id` (the preview is not a project file) or `publish_failed` (select or draw again). |

## What is not built

- **Codex sessions.** A Codex session takes neither a selection nor a drawing; the status line says
  `not shared: this row's agent takes none` for both.
- **Drawing over a PDF page, an SVG or a video frame.** `d` works over image files only; draw on a PNG of the page or
  on the canvas instead.
