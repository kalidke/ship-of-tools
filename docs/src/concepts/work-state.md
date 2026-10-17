# Work-state colors

With several agents running, the question is always the same: *which one needs
me?* Ship of Tools answers it with color. Every session row in Sessions mode
and on the bottom session strip is tinted by that agent's current work state,
so you scan one list instead of tabbing through panes.

```@raw html
<DemoShot path="readme/sessions-crop.png" caption="Demo session rows tinted by work state; the bracketed [ready] means the session process is up, and the text after it is the reason sent with the state." />
```

## The five states

The colors are the four Julia brand colors plus gray.

```@raw html
<table>
  <thead><tr><th>Color</th><th>State</th><th>Meaning</th><th>What to do</th></tr></thead>
  <tbody>
    <tr><td><span class="sot-state working"></span>green</td><td><strong>working</strong></td><td>a turn is running</td><td>nothing; let it work</td></tr>
    <tr><td><span class="sot-state question"></span>red</td><td><strong>blocked</strong></td><td>waiting for your answer: the agent stopped with a question for you</td><td>answer it; typing into the session clears the red</td></tr>
    <tr><td><span class="sot-state waiting"></span>purple</td><td><strong>waiting</strong></td><td>the agent handed off to a peer or a long job and is waiting on the result</td><td>nothing yet; it resumes on its own</td></tr>
    <tr><td><span class="sot-state done"></span>blue</td><td><strong>done</strong></td><td>a turn you started has finished and you have not looked at it</td><td>read the result</td></tr>
    <tr><td><span class="sot-state idle"></span>gray</td><td><strong>idle</strong></td><td>nothing running, nothing pending</td><td>nothing</td></tr>
  </tbody>
</table>
```

One marker sits outside the five states. When an agent opens a result in your
preview pane from a session you are not viewing, that session's row turns
bright white with a leading `●` until you switch to it. The bottom strip orders
the rows on each host red, white, blue, green, purple, then gray, so what needs
you comes first.

A `working` stamp that is not refreshed for 10 minutes is dimmed as stale, so a
session that died mid-turn does not look busy forever.

## Where the colors come from

A row's state is not a single value the agent sets; it is a small set of
**facts** reduced by priority:

- hook **events** mark each turn's start and end (and who started it — you, or
  a message from another agent);
- the agent's own **declarations** — `working`, `blocked "question"`,
  `waiting "on what"`, `done`, `idle` — set or clear a question, a wait or a
  done flag. `waiting` clears an open question (it says nothing needs you), and a
  `blocked` or `waiting` with no text, and none of its own already on the row, is refused.

The display picks the highest-priority fact that holds: a dialog that waits on
you is **red**, even while its turn runs; an open question with no turn running
is **red**; otherwise a running turn is **green**; otherwise a wait is
**purple**; otherwise an unread result is **blue**; otherwise **gray**. Red wins
whenever it holds: a dialog at once, a question you must answer once the running
turn ends. Typing into a session (which starts a turn you own) clears both red
and blue, and looking at a blue row for 10 seconds marks it read. A Codex
session's permission request raises the dialog. For a Claude Code session, which
sends no event when a dialog shows, the backend reads the session's screen every
few seconds; on Windows it does not, and such a session still reads green.

These facts live in the comm registry, next to each session's handle, which is
why another agent — and every frontend attached to the backend — sees the same
color you do. An agent sends a declaration with `comm-status.sh`, for example
`comm-status.sh blocked "<question>"`; the usage is in the [Comm protocol](../ref/comm.md).

The per-entity *provenance* colors (who edited this function, is it accepted)
are a separate, not-yet-built layer: see
[Provenance colors (design)](../guide/color-coding.md).
