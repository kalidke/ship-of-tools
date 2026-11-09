# Previews

When you cursor a file in any mode, the preview pane renders it at the fidelity
appropriate to its type. The backend first asks the Julia kernel: a
[`FileType`](@ref) plugin claims the path with [`matches`](@ref) and returns a
[`PreviewPayload`](@ref) from [`preview`](@ref). When no plugin claims it, the
backend reads the file itself; that is how raster images and plain text are
served. The payload's `mime` tells the
frontend which renderer to use; the bytes are opaque to Rust. Adding a format is a
Julia-only change — see [The Dispatch ABI](../extend/abi.md) and
[Writing a FileType Plugin](../extend/filetype.md).

Drag with the left mouse button to select text in a rendered markdown, source or plain text preview, then press
Ctrl+C (Cmd+C on macOS) to copy it. Ctrl+Shift+C (Cmd+Shift+C on macOS) also copies the selection; `y` copies all
fenced code blocks in a markdown preview.

```@raw html
<DemoLoop name="pdf" caption="With the preview focused, n and p page through a PDF, then = zooms in to read it." />
```

```@raw html
<DemoShot name="preview-hdf5" caption="An HDF5 file: groups, datasets, shapes, element types and attributes." />
```

## Built-in file types

These plugins ship under `julia/plugins/` and the kernel loads them at startup.
Each is a built-in that travels the exact same dispatch path a third-party plugin
would.

| Extension | Representation | Fidelity |
|-----------|----------------|----------|
| `.jl` | token spans (kind + text) from `JuliaSyntax.jl` | syntax-highlighted source, no client-side re-tokenizing |
| `.md`, `.markdown` | raw UTF-8 bytes | rendered markdown (comrak + cosmic-text), including inline math |
| `.toml` | raw UTF-8 bytes | code/text renderer |
| `.json` | raw UTF-8 bytes | plain text today (pretty-print / colour planned) |
| `.txt` | raw UTF-8 bytes | plain text renderer |
| `.pdf` | one poppler-rasterized page | paged; `n`/`p` turn pages with the preview focused, rasterized backend-side to fit the pane |
| `.mp4`, `.webm`, `.mov`, `.mkv`, `.m4v` | a single ffmpeg poster frame | still poster in the pane; `o` opens playback in the browser |

The plugin module and MIME type behind each row are listed under
[Built-in preview plugins](@ref builtin-preview-plugins).

PNG (and other raster images) render directly through the frontend's image quad
path — the same path the PDF and video plugins reuse by returning `image/png`.

A PNG's embedded pHYs density supplies a scale bar only when both axes have the same metre-unit density of at least
100,000 pixels per metre (10 micrometres per pixel at the cutoff). Lower densities, including CairoMakie's print DPI,
give no scale bar. A calibrated `.scale.json` sidecar takes priority; use `Primary+s` to enter the pixel size in nm
when scale is unknown.

A few notes on the table:

- **Julia source** is tokenized on the kernel side into `{spans: [{text, kind}]}`,
  so the frontend colours it without parsing Julia. The token kinds map to the
  frontend's colour set (`keyword`, `comment`, `string`, `number`, `op`, `punct`,
  `type`, `ident`, `text`); concatenating every span reproduces the file
  byte-for-byte.
- **PDF** is the one format whose preview takes a page parameter — the only
  built-in that addresses *which part* of a file to render. Pages rasterize on the
  host where the file lives (poppler's `pdftoppm` / `pdfinfo`), matching the
  backend-side-decode model.
- **Video** deliberately does *not* play in the pane. A native player (HTML5
  `<video>` with hardware decode) beats streaming decoded frames over a socket, so
  the pane shows a poster and `o` pops the real file out to the browser.

When an external tool is missing (ffmpeg for video, poppler for PDF), the plugin
returns a `text/markdown` note explaining the gap — never a silent blank pane.

## Opens in the browser, not the pane

Some formats are interactive HTML and belong in a real browser rather than a
static pane. Consistent with the video policy, these are popped out to the OS
browser through the backend connection, local or remote, rather than rendered
in-pane:

- **HTML** (`sample.html`) — opened in the browser, with the figure and movie
  beside it served by the daemon's page server, so a missing one shows the page
  path dropped it.
- **Pluto notebooks** (`pluto_demo.jl`, `pluto_demo2.jl`) — opened as live Pluto
  sessions in the browser. As `.jl` files they still get a syntax-highlighted
  source preview in the pane.
- **Quarto documents** (`quarto_julia.qmd`) — the rich rendered form opens in the
  browser; the source previews as text in-pane. On Linux the Julia engine server a
  render starts ends when the backend daemon ends; a `quarto render` of your own that
  found and reused that server (Quarto finds it through `XDG_RUNTIME_DIR`) starts a
  new one after that.

These all follow the same "rich/interactive content lives in the browser" policy.

## A page that acts

A page opened by `W` or `sot-fe docs` can read session states and act on rows of the daemon serving it. Its script
posts JSON to its own URL:

```javascript
const response = await fetch(location.pathname + location.search, {
  method: "POST",
  headers: {"Content-Type": "application/json"},
  body: JSON.stringify({verb: "states"})
});
const answer = await response.json();
```

`states` answers `{ok: true, rows: [...]}`, sorted by handle, including sessions outside rows. Each entry has
`handle`, `host`, `repo`, `row`, `state`, `summary`, `question`, `options` (objects with `key` and `text`), `recommend`,
`page`, `waiting`, `status_at` and `last_seen`. An absent string is empty, and `row` is empty for a session outside any
row. A question's `Options:`, `Recommend` and `Page:` parts are separated into those fields. Only this daemon's host
entries are listed. An unreadable or damaged registry answers 500 `handler_error`; an empty `rows` array means a
successful read found no sessions of this daemon.

Post `{verb: "goto", handle: "session-handle"}` to switch the window that opened the page to that row. Post
`{verb: "open", handle: "session-handle"}` to open the page the row's question names. The daemon reads the question
again and requires an absolute page under that row's project folder. It grants a site rooted at that row and checks
each file request there. Unix grants hold the row folder by identity and open files relative to that held folder;
Windows retains a path-based check/open race. The page
script supplies a handle; it cannot select an arbitrary file. These actions need no confirmation and answer
`202` with `{ok: true}`. They go to the page's own window even when another window is in use, locally and through a
remote window's page proxy. `{verb: "open", handle, embed: true}` instead answers `200` with `{ok: true, url}`, a
path on the calling page's own origin for an `<iframe>`, and opens nothing: the framed page can be read but cannot
answer, and a page keeps one embed at a time, so a new one replaces the last.

Replies are JSON and never cached. A refusal has `error` (the explanation) and `code`: `not_a_row` (409, including a
foreign session, whose daemon is named), `no_page` (409), `page_outside` (403), or `window_gone` (503). States still
works while the window is away. Missing fields, malformed JSON and unknown verbs answer `bad_request` (400).
Post `{verb: "reply", handle: "session-handle", text: "approve"}` to answer that session. Text trims ASCII spaces,
must be nonempty and fit in 512 UTF-8 bytes, and must contain no controls, line separators or invisible formatting
characters. Invalid text answers 400 `bad_text`, naming the refusal. Accepted text gains the visible `[sot-page] `
mark. Only a page opened with a reply scope answers: `sot-fe docs --direct` gives a session's own page its row,
`--direct-all` gives a list page every row, and a question's page opened by `open` answers its own row. Such a reply
is typed at once at the session's free prompt, or held until the prompt frees, and answers 200 with its receipt:
`sent`, `held`, `refused` with the reason, or `unknown`. A page opened without a scope is refused, 403
`no_reply_scope`, and nothing is typed or offered anywhere. Every reply leaves one line in the daemon's log, whatever
became of it.

The typing rechecks the mark and the target handle and uses the wake's free-prompt gate, sharing one typing attempt
per row. It refuses a dialog, a menu, unsent text a person typed (the refusal quotes a one-line draft) and an
unsupported agent, and holds a reply for a
working screen or a running Stop hook; only a Ready Claude capsule row is supported. A line wider than the pane wraps
and is checked across its rows before Enter. A sent line carries the mark in the pane and the transcript.
`typed_not_sent` means text was typed but Enter withheld, and `unknown` means delivery could not be confirmed; inspect
the pane before trying again. An unsent page line is never completed by the wake. A card answer counts as the
person's word, a merge, a tag or a registration in General included, behind the card's acknowledgment of what it
does. The mark is not proof of physical keyboard input, since existing `pty.input` callers can type text.

The channel requires the site's live nonce or explicit pool query secret and a matching browser Origin. Keep
`location.search` in a pool page's POST URL: cookies alone do not authorize verbs, since another open may replace
the browser's cookie while an old tab remains. Other origins and
requests without Origin are refused (403); OPTIONS cannot authorize a cross-origin request (405). POST bodies need a
Content-Length and fit within 4 KiB; valid requests reuse their connection for polling. The last eight prefix pages
opened by a window keep working, so a queue page can open a decision page and keep polling. A site with root-relative
links uses a pool port instead, with one site per window: opening another pool site replaces it.
Decision pages opened by a verb must use relative paths for assets and navigation, such as `assets/app.js`;
root-relative paths such as `/assets/app.js` are unsupported and normally return 404.
All page listeners share separate budgets of four body readers, four states handlers and four action handlers;
stalled actions cannot consume polling slots. Excess requests answer 503 `server_busy`. A handler exceeding five
seconds answers 503 `handler_timeout`; stalled blocking work keeps its class's slot until it ends. These errors have the same
JSON `error` and `code` fields and can be shown by the page.

The fixture at `rust/backend/tests/fixtures/page_channel/index.html` lists states every three seconds and provides
Go to, Context and reply controls for checking the channel in a local or remote window.

A page can also show data that lives outside its repo. When the repo tracks a
symlink to a folder on a mounted share or drive (under `/mnt`, `/media` or, on
macOS, `/Volumes`), images and movies on the page load through that link, and
movies seek; nothing needs to be declared. Write the link as the share's absolute
path, as `ln -s /mnt/<share>/... <name>` does: a relative target, or one that runs
through another link or a `..`, is refused, and so is a symlink inside the share: a
link serves only the files directly under its target folder, by their own names. A file in your home folder, or with a
folder or name below the mount folder, on the file's real path, that starts with `.`,
is never served through a link (a link itself named `.data` still serves).
Anything else is refused with a message saying why, which the browser shows when
the missing file is opened in a tab of its own: a link git does not track (`git
add` it and reload), a linked file that is not under those folders or does not
exist on the computer whose daemon serves the page (is its share mounted?), a
link to `/`, to your home folder or the repository or a folder inside it (in any letter case), to a link whose target is not written as the share's absolute real path, a
`..` in the path, `.git`, a repo file outside the page's site folder (the
outermost folder above it, within its repository, that holds an `index.html`),
or a file that changed while it was being
opened. On macOS, following a link needs git, which means the Command Line
Tools; the daemon picks up tools installed later at its next restart. On Windows
no link is followed, so a page there never shows linked data.

## Pan and zoom images

With the preview focused (`Ctrl+Right`), `=` zooms an image in and the arrow
keys pan it. Same-size images in a directory share zoom and pan, so stepping through a run's plots keeps your
framing.

```@raw html
<DemoLoop name="zoom" caption="With the preview focused, = zooms an image and the arrow keys pan; the next same-size image opens at the same zoom and pan." />
```

## Drawing on a preview

With the preview focused, `d` turns drawing on over a raster image. Holding the left button draws and holding the
right button erases the strokes the pointer crosses; `u` (or `Ctrl+z`, `Cmd+z` on macOS) undoes the last step, and `Delete` clears all the
ink (`u` brings it back). The ink stays where you drew it as you zoom and pan. `Shift+d` shows a blank canvas to draw
on, and shows the preview again; the canvas is always in drawing mode. Each session keeps one drawing, and a stroke on
another image or the canvas replaces it, with a note on the status line.

The session's agent sees what you draw: when you release a stroke, the window tells the daemon, which writes one
picture of the image with the ink on it inside the project and names it to the agent at its next prompt. Erasing all the
ink, showing another file, or switching session withdraws it; showing the image or the canvas again shares it again.
The status line says when the drawing was shared, held (the agent started before it could take one; restart the session),
or not shared. Claude Code sessions take a drawing; Codex sessions take none yet. A closed window loses its drawings.

## Sharing a selection with the agent

Select text in a preview of a project file with the mouse and release the button: the window tells the daemon, which
reads the file itself and keeps the lines you selected for the session's agent. The agent is told at its next prompt,
including a prompt a mail wake types and a message queued while a turn runs; the line `Selection: <file> lines a-b`
shows in its pane, and the agent is given the file's own text at those lines (the first 2000 characters of a longer
range, with its size), labeled as your selection. Nothing is sent while you drag, and every later prompt carries the
selection until it is withdrawn.

Only the preview's text of a file in the project is shared. The agent pane, an image, the concept pane and a
rendered table are not. A selection is shared only if the preview still shows the file's own text: a preview older than
the file, rendered markdown whose text differs from the file's, and files with lone CR, U+2028, U+2029 or U+0085 line
breaks are refused. Drawing over an image in the same session replaces the selection with the drawing.

A selection is withdrawn when you click outside the text, press `Esc` in the preview, show another file or open the
editor, switch to another session (the session you left is cleared), or close the window. A connection that ends (a
lost link, a closed window) withdraws its selection in the daemon, and a window that reconnects shares its current
selection again. A selection the agent was already given stays in that conversation's history: asked again, it still
names it. Only a new conversation forgets it.

The status line says what happened:

- `selection shared: the agent sees it at its next prompt`
- `selection held: this row's agent started before it could take one; restart the row to share`: the agent was started
  without the hook (before the release that brings it, or with its own `--settings`), so it sees the selection after
  the session is restarted.
- `selection not shared: this view is not the file's text`
- `selection not shared: this row's agent takes none`
- `selection not shared: this host's daemon does not take selections`
- `selection not shared: <reason>` for any other refusal, such as `unknown_workspace` or `publish_failed` (select
  again).

Withdrawing is not announced. Claude Code sessions take a selection; Codex sessions take none yet, and a Codex session
answers `held` (the file is kept, but no hook tells its agent). A Claude Code session that runs `claude -p` or
`codex exec` through its shell tool gives that child the same selection in its own prompts. A closed daemon leaves the
selection in place until a daemon starts again. A selection set from two windows is the newest one.

## Capturing a preview region for the LLM

For raster image previews you can zoom in, then, with the preview focused
(`Ctrl+Right`), press `c` to crop the visible
region and hand it to the session's agent in the agent pane, with a question
such as "what's the artifact here?". The crop is taken from
the source image on the backend and delivered to the agent, so it
sees exactly the region you are looking at.

## Formats added by external plugins

File types do not have to be built-in. The [HDF5 worked example](../extend/hdf5.md)
ships as a separate package and claims `.h5` (see the `sample.h5` fixture) with no
Rust changes. It is a dependency of the kernel's environment and is registered in
the kernel's lazy-load table (`LAZY_PLUGIN_FOR_EXT`), which loads it on the first
`.h5` preview.

## Adding a new file type

A `FileType` plugin is small: a subtype, a [`matches`](@ref) predicate, and a
[`preview`](@ref) that returns a [`PreviewPayload`](@ref) with the right MIME.

```julia
module CsvPreview

using ConceptExplorerCore

struct CsvFile <: FileType end

ConceptExplorerCore.matches(::Type{CsvFile}, path) =
    endswith(lowercase(path), ".csv")

function ConceptExplorerCore.preview(::Type{CsvFile}, path)
    lines = readlines(path)
    ncols = length(split(lines[1], ","))
    table = String[]
    for (i, line) in enumerate(lines)
        push!(table, "| " * replace(line, "," => " | ") * " |")
        i == 1 && push!(table, "|" * repeat(" --- |", ncols))  # header rule
    end
    PreviewPayload("text/markdown", Vector{UInt8}(join(table, "\n")))
end

end
```

The plugin runs in the kernel, not in your REPL: add the package to the
`julia/kernel` environment (`Pkg.develop` or `Pkg.add`), then add it to the
kernel's `using` list or map its extension in `LAZY_PLUGIN_FOR_EXT`. Today that
works only from a source checkout. No Rust change is needed. See
[Discovery](../extend/discovery.md) for what loads today. For the full contract and discovery rules see
[The Dispatch ABI](../extend/abi.md) and [Writing a FileType Plugin](../extend/filetype.md).
