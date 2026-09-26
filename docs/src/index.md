```@raw html
---
layout: home

hero:
  name: "Ship of Tools"
  text: "An agentic development environment for Julia"
  tagline: "You and the agent share one Julia REPL; figures render in the window; sessions keep running on the server."
  actions:
    - theme: brand
      text: Get started
      link: /start/quickstart
    - theme: alt
      text: What it does
      link: /features
---
```

```@raw html
<div class="sot-section sot-hero-media">
  <DemoLoop name="hero" caption="Asked for a bar chart of the route legs, a Claude Code session edits scripts/route.jl, runs it in the shared Julia REPL and shows the chart in the preview (navigation column cropped, sped up)." />
  <p class="sot-why">A desktop window, drawn on the GPU, with a keyboard-driven layout: a navigation tree, a file preview, an agent pane and a Julia REPL drawer.</p>
  <p class="sot-why">CairoMakie figures appear inline; interactive WGLMakie figures and Pluto notebooks open in your browser. Because the window draws all of this itself, it needs a graphical desktop, not a terminal. Linux; Windows and macOS run as frontends to a Linux backend (macOS is experimental). See <a href="./start/install#platforms">Platforms</a>.</p>
</div>

<div class="sot-section">
  <h2>Several agents, one list</h2>
  <p class="sot-why">Every agent session is a row in Sessions mode, on this machine or on any connected host. The row colour is the agent's <a href="./concepts/work-state">work state</a>, so you can see which session needs you without opening each one. <code>Shift+Arrow</code> switches sessions; each session keeps its own Julia REPL, and switching never restarts it.</p>
  <DemoLoop name="sessions" caption="Demo session rows changing colour as a script sets their work states: green working, red waiting for your answer, purple waiting on a job, blue done, gray idle. Each row's ready tag means its supervisor has a live agent process running." />
</div>

<div class="sot-section">
  <h2>Navigate the project, see the file</h2>
  <p class="sot-why">The navigation pane is a tree whose root you switch with one key: <code>f</code> for files, <code>m</code> for modules and their definitions, <code>s</code> for sessions, <code>h</code> for hosts. The preview pane renders the entry under the cursor: markdown with typeset math, paged PDF, images, HDF5 structure, syntax-highlighted Julia.</p>
  <DemoLoop name="navigate-g" caption="The preview follows the cursor through notes with math, the agent's bar chart, an HDF5 file, a PDF and the route figure." />
</div>

<div class="sot-section">
  <h2>Your machines as one workspace</h2>
  <p class="sot-why">The backend runs where the data and GPUs are; the window runs on your laptop or desktop and attaches over SSH. Sessions, REPLs and agents keep running on the server when the lid closes or the window restarts, and several windows can attach to one backend. See <a href="./start/remote">Going remote</a>.</p>
</div>

<div class="sot-section">
  <h2>Extend in Julia</h2>
  <p class="sot-why">A preview for a new file type is a Julia plugin: a <code>FileType</code> subtype, a <code>matches</code> method and a <code>preview</code> method, with no Rust changes. This one shows a CSV file as a markdown table. The built-in text, source, PDF and video previews use the same interface. Today a third-party plugin loads only from a source checkout; see <a href="./extend/discovery">Discovery</a> and the <a href="./extend/hdf5">HDF5 preview tutorial</a>.</p>
</div>
```

```julia
struct CsvFile <: FileType end

ConceptExplorerCore.matches(::Type{CsvFile}, path) = endswith(lowercase(path), ".csv")

function ConceptExplorerCore.preview(::Type{CsvFile}, path)
    # ... build a PreviewPayload of "text/markdown" from the CSV rows
end
```

The full walkthrough, with the table-building body, is in [Writing a FileType Plugin](./extend/filetype.md).

```@raw html
<div class="sot-section">
  <h2>Install</h2>
  <p class="sot-why">Frontend and backend together on one Linux machine. For a remote backend, a second frontend or Windows, see <a href="./start/remote">Going remote</a> and <a href="./start/install">Install details</a>, which also lists what the installer writes and how to remove it. What it touches:</p>
  <ul class="sot-why">
    <li>Everything goes under your home directory, mostly <code>~/.local/share/sot</code>, plus user lingering so the backend keeps running after logout on Linux.</li>
    <li>Skills and hooks go into your global <code>~/.claude</code> and <code>~/.codex</code>; a skill of yours with the same name as a shipped one is overwritten without a backup.</li>
    <li>Sessions start their agent with fixed permission flags: Claude Code in auto mode; Codex with approvals, the sandbox and hook trust all bypassed.</li>
  </ul>
  <p class="sot-why">No isolated mode yet: try it under a separate user account or in a VM. See <a href="./start/install#install-footprint">What the installer changes</a>.</p>
</div>
```

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/scripts/install.sh | bash -s -- --local
```

```@raw html
<div class="sot-section sot-tight">
  <p class="sot-why">Or ask a coding agent on the target machine:</p>
</div>
```

```text
Install Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md and follow it.
```

```@raw html
<div class="sot-section">
  <h2>Read next</h2>
  <ul>
    <li><a href="./features">What it does</a> — every feature, most with a short recording.</li>
    <li><a href="./start/quickstart">Quickstart</a> — install, launch, start an agent session.</li>
    <li><a href="./start/tour">Your first session</a> — the panes, the agent, drawers and modes, one at a time.</li>
    <li><a href="./guide/architecture">Architecture</a> — the frontend, the daemon, the Julia kernel and the agent sessions.</li>
    <li><a href="./design/roadmap">Roadmap</a> — what is built, and what is planned.</li>
  </ul>
  <p class="sot-why">Status: in active development and used daily with a Linux backend and a Windows frontend (installed from the release zip, no installer script yet).</p>
</div>
```
