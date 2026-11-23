# Drawing and selecting for the agent

A session's agent can be given what you mark in the preview pane: a drawing over an image or a page of a PDF, a
drawing on a blank canvas, or text you select. It goes to the selected session, the one the agent pane shows, with your next prompt, so
you can circle a peak in a figure and ask what it is, or select a paragraph and ask for a shorter one. This page calls
a drawing or a selection *shared* while the agent is given it.

Claude Code sessions and Codex sessions on Unix accept text and drawings. Sharing a drawing replaces any text
selection. Sharing text replaces the drawing.

## Before you start

- **Keys.** Keys are written as on Linux and Windows; on a Mac, press Cmd where this page says Ctrl. A letter in a key
  is always lowercase: `Shift+d` is Shift and d.
- **Which pane has the keys.** The window's three columns are the navigation tree, the preview pane and the agent
  pane, and one pane at a time has the keys: its title bar shows its name and its keys. `Ctrl+ArrowRight` gives the keys
  to the pane on the right and `Ctrl+ArrowLeft` to the pane on the left, so from the navigation tree one
  `Ctrl+ArrowRight` reaches the preview and a second reaches the agent pane. A mouse press on text in the preview
  gives it the keys too.
- **The drawing keys belong to the preview.** `d`, `u`, `Delete` and `Shift+d` do what this page says only while the
  preview has the keys. In the navigation tree `d` downloads the selected file, `u` uploads files, and `Shift+d`
  pressed twice on a session in Sessions mode ends that session. Look at the preview's title bar before you press
  them.
- **Words in the window's messages.** The messages quoted on this page say `row` for a session, and `daemon` and
  `host` for Ship of Tools' service (`sotd`, the background program that runs the sessions) and the computer it runs
  on.

## Drawing on an image

1. With the navigation tree holding the keys (`Ctrl+ArrowLeft` brings them back from another pane), press `f` for
   Files mode and move to an image file with the arrow keys: PNG, JPEG, GIF, BMP, WebP or TIFF. The preview shows it.
2. Press `Ctrl+ArrowRight` to give the preview the keys, then `d`. A box at the foot of the preview says
   `drawing on: press and drag on the image; u undo, Delete clear, d off`.
3. Hold the left mouse button and drag over the image. A red line follows the pointer.
4. Release the button. The box says `drawing shared: the agent sees it at its next prompt`.
5. Press `Ctrl+ArrowRight` again to give the agent pane the keys, type your question and send it. Claude Code shows
   a line such as `UserPromptSubmit says: Drawing: plots/peaks.png (1 stroke)`, and the agent is told where to read
   the image with your drawing on it; [What the agent receives](#What-the-agent-receives) has the details.

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

Nothing on screen marks that drawing is on once the box has faded. To check, drag on the image: a red line appears
only while drawing is on, and `u` removes it.

## Drawing on a page of a PDF

Show a PDF in the preview and draw on the page it shows exactly as on an image: `d`, then drag. The keys and buttons
of the table above work the same, and `n` and `p` still turn the page.

- **The drawing belongs to its page.** Turn the page and the ink is not shown and no longer shared; turn back and it
  is shown and shared again. The first stroke on another page replaces it, as described under
  [One drawing for each session](#One-drawing-for-each-session), and the window says
  `the drawing on <file> page <n> was replaced`.
- **Zoom.** Zooming in shows the page sharper a moment later. The ink stays where you drew it on the page, also when
  that happens in the middle of a stroke.
- **What the agent gets.** A picture of that page with your ink on it, and the PDF's path and the page number. The
  picture is drawn anew for the agent, 2000 pixels on its longer side whatever your zoom, so very small print can be
  hard to read in it; the agent can also open the PDF itself.
- **Where the session runs.** The computer that runs the session draws the page, with the same program that shows it
  in the preview. If that computer's Ship of Tools is too old to draw on a PDF page, the window says
  `drawing not shared: this daemon cannot draw on a PDF page; update it`, and nothing of that session stays shared.
- **A PDF that is rebuilt.** When the PDF is written again while you have a drawing on one of its pages, the preview
  shows the new page with your ink in the same place, but the agent still has the picture of the old page. Draw
  another stroke, or undo one, to share the page as it is now.

## The canvas

With the preview holding the keys, `Shift+d` covers the preview with a blank white page, whatever file it showed. The
box says `canvas: draw with the pointer; u undo, Delete clear, Shift+d back`. Drawing is always on there, and the two
mouse buttons, `u`, `Ctrl+z` and `Delete` work as in the table above.

`Shift+d` again shows the preview as it was, and so does moving to another file. While the canvas is hidden its
drawing is kept but not shared; showing the canvas again shares it again.

## One drawing for each session

A session has one drawing, on one image, on one page of a PDF or on the canvas. The first stroke anywhere else
replaces it, a hidden canvas's drawing included, and the window says `the drawing on <name> was replaced`.

Each session keeps its own drawing, and whether drawing is on, while you work in another session. Closing the window
loses every session's drawing, and no agent is given one after that.

## Selecting text

1. Show a text file of the project in the preview: markdown, source code or plain text.
2. Drag over the text with the left mouse button, and release. The status line, the line starting `status:` at the top
   of the navigation tree, says `selection shared: the agent sees it at its next prompt`.
3. The drag gave the preview the keys, so one `Ctrl+ArrowRight` gives them to the agent pane. Type your prompt and
   send it.

Nothing is sent while you drag. The highlight is the one `Ctrl+c` copies; in a rendered markdown preview the agent is
given more than the highlighted words, as described below.

Only text of a file in the project, selected in the preview, is shared. A selection in the agent pane, in a
[concept annotation](concept-layer.md) or in a rendered table is not, and nothing can be selected in an image.

In a rendered markdown preview the agent is given lines of the markdown file itself, not the rendered words:

- every paragraph or heading the selection touches, whole (in a list, the paragraph of each item it touches);
- in a code block, the lines the selection touches;
- any table, equation or figure that lies between the parts it touches;
- link definitions (`[name]: url` lines) that stand just above a paragraph it touches.

A selection that touches nothing but a table, an equation, a figure or a horizontal rule is not shared.

The window says `selection not shared: this view is not the file's text` when the file changed after the preview read
it, and when the file, before the end of the selection, breaks a line in an unusual way: with a carriage return alone
or with one of Unicode's line separators.

## What the agent receives

When you release a selection or a stroke, the window tells Ship of Tools' service on the computer that runs the
session. At your next prompt Claude Code shows one line in the agent pane: its own words `UserPromptSubmit says:`,
then a label of what was shared. On Unix the Codex prompt hook supplies the labels and content below. For a
drawing it asks the agent to open the marked PNG. The visible notice and the agent reading the marks still need
checking in a live Codex window.

| You shared | The label | The agent is given |
|---|---|---|
| A selection | `Selection: <file> lines <a>-<b>` | The text of those lines that you selected (in a markdown preview, the lines listed under [Selecting text](#Selecting-text)), which the service reads from the file itself, labeled as your selection with the file and its line numbers. |
| A drawing on an image | `Drawing: <file> (<n> strokes)` | The image's path and the path of a picture of the image with your ink on it, with the request to read the picture to see what you marked. |
| A drawing on a page of a PDF | `Drawing: <file> page <p> (<n> strokes)` | The PDF's path, the page number and the path of a picture of that page with your ink on it, with the same request. |
| A drawing on the canvas | `Drawing: sketch (<n> strokes)` | The path of a picture of the canvas, with the request to read it. |

Every later prompt carries it again for as long as it is shared.

- **A long selection.** The agent is given the first 2000 characters of what is shared and is told how long the whole
  was. In a markdown preview whole paragraphs are shared, so in a long paragraph the cut can fall before the words you
  selected.
- **The picture.** The service writes it under `.sot/captures/` in the project's folder, as one file per session
  named `ink-` and the session's internal id, and writes it again at each change to the drawing. The agent is told the
  path, so you never need the id. The picture is a PNG copy of the image with the ink on it, scaled down to 2000
  pixels on its longer side when the image is larger; for a page of a PDF it is that page drawn at 2000 pixels on
  its longer side. The image or PDF file itself is never changed. The picture stays in the
  folder after the drawing stops being shared, until the session's next drawing writes over it; add `.sot/captures/`
  to the project's `.gitignore` if git should not see it.
- **One thing at a time.** The agent is given a selection or a drawing, never both: the newest replaces the other.
  When two windows show one session, the newest from either window is the one shared, and when either window stops
  sharing it, it is no longer shared from the other.

## What stops the sharing

A drawing or a selection is shared while it is on screen in the selected session's preview: the ink on the shown image,
page or canvas, the highlight on the selected text. When it leaves the screen the agent is no longer given it, and the
window shows no message for that. To be sure of what the agent has, look at the preview before you send a prompt.

- **To stop sharing a selection:** click in the preview, or press `Escape` with the preview holding the keys. That
  `Escape` also does what `Escape` does there otherwise: it restores a maximized pane or the wide preview when one is
  on, and if neither is, gives the keys back to the navigation tree.
- **To stop sharing a drawing:** clear it with `Delete`, or erase or undo until no ink is left.

These remove the highlight of a selection, and so stop sharing it; select again afterwards:

- showing another file, or switching to another session;
- opening the editor (`e`);
- a change to the file, which refreshes its preview, or to the text size;
- in a markdown preview, an equation, a figure or a code block's colors appearing, each of which lays the document
  out again. They appear a moment after the file is shown, so select once the document has settled.

These hide a drawing, and so stop sharing it until its image, its page or the canvas is shown again in that session,
when it is shared again by itself:

- showing another file, another page of the PDF, or the canvas over the image;
- opening the editor (`e`);
- switching to another session.

A file written again while its drawing is on screen does not stop the sharing, and does not renew it: the ink stays
over the new image or page, and the agent keeps the picture made when the drawing was last shared, of the file as it
was then, until you change the drawing or hide it and show it again.

A selection under the canvas is kept, and is shared again when the canvas is hidden.

Two more things stop the sharing whatever is on screen. When the window loses its connection to the computer that runs
the session, nothing is shared until the connection is back, and then the window shares again what it shows. And when
the window says a new selection or drawing was not shared, the one shared before it is no longer shared either.

What the agent was already given stays in that conversation's history: asked later, it can still say what it was
given. A new conversation forgets it.

## When the window says `held`

`selection held: this row's agent started before it could take one; restart the row to share`, and the same words
for a drawing, mean the session's agent was started without the part of Ship of Tools that hands it a selection or a
drawing. Every session started on 0.7.0 stays in this state after the update to 0.7.1. On an updated service,
a Codex session launched without the selection hook answers `held` for either text or a drawing.

End the session (`Shift+d` twice on it in Sessions mode) and start a new one in the same folder, then select or draw
again. Use `/resume` in the new session to return to an earlier conversation. If a new session still says `held`,
run the installer on the computer running the session, then restart the session and share the text or drawing again.
Codex also needs a version that accepts `--no-daemon` for that launch. See [Updating](updating.md).

## Other things the window says

A message about a drawing shows on the status line and in the box at the foot of the preview; a message about a
selection shows on the status line alone, and a `Can't draw` message in the box alone.
[Messages about the preview](previews.md#Messages-about-the-preview) says how long the box stays. The message for `d`
over a picture that is not an image file is under [What is not built](#What-is-not-built).

| The window says | What it means |
|---|---|
| `the drawing is full: undo or clear` | The drawing holds no more ink. Undo or clear to draw more. |
| `Can't draw here: there is no image file to draw on.` | `d` where the preview shows no image file. |
| `Can't draw while the connection notice covers the preview.` | The preview is covered by a notice that this window and Ship of Tools on the session's computer cannot work together, usually because one of them needs updating. The notice says why; nothing can be drawn until it is gone. |
| `selection not shared: this row's agent takes none`, `drawing not shared: this row's agent takes none` | Use a Claude Code session or, on Unix, a Codex session. |
| `selection not shared: this host's daemon does not take selections`, `drawing not shared: this daemon cannot take a drawing; update it` | The computer that runs the session has an older Ship of Tools. Update it. |
| `drawing not shared: the image is over 40 megapixels` | The image is too large to draw on. |
| `drawing not shared: the daemon cannot read this image` | The service could not read the file as a picture. |
| `drawing not shared: this daemon cannot draw on a PDF page; update it` | The computer that runs the session has a Ship of Tools that draws on images only. Update it. |
| `drawing not shared: the daemon could not render this page` | The service did not draw the PDF page under your ink: the file or the page is gone, or the program that draws pages did not answer in time. Draw another stroke to try again. |
| `selection not shared: <reason>`, `drawing not shared: <reason>` | Any other refusal, with the service's short word for the reason. Select or draw again; if it stays, that word is what to report. |

## What is not built

- **Drawing over an SVG or a video's poster frame.** `d` works over image files and PDF pages only. Over any other
  picture it draws nothing, and the box says, for an SVG:

  `Can't draw on this SVG. Drawing (d) marks up an image file (PNG, JPEG, TIFF and the like) or a PDF page for this row's agent, which sees your marks with your next prompt. To draw anyway, open a PNG of it, or press Shift+d for a blank canvas.`

  To draw on such a picture, save it as a PNG (plotting code can usually save one beside the SVG) and draw on that, or
  draw on the canvas.
- **A drawing kept for each page of a PDF.** A session has one drawing, so drawing on a second page replaces the
  first page's.
