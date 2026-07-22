# ADR 0025: Daemon-authoritative FE — imperative commands + FE-as-viewport

**Status:** Accepted (co-design converged 2026-06-22; building on `feat/op-fe-command`)
**Date:** 2026-06-22

> **Update — 2026-07-21: `preview` gains an optional `roi` (source-px viewport aim).**
> A session can aim the FE viewport at a rectangle of the SOURCE image:
> `preview{ws, path, roi?}` where `roi = {x, y, w, h}` in source-image pixels
> (offset from top-left) — the SAME vocabulary as ADR-0022's `image.crop`, so a crop
> taken now round-trips to "look here again" later (identical rect ⇒ identical region
> on any display; source-px is DPI- and pane-size-independent, unlike raw zoom/pan).
> **Split:** the BE owns the request side (`sot-fe preview <ws> <path> --roi x,y,w,h`,
> validated CLI-side: four non-negative integers, `w,h > 0`); the FE owns the solve —
> it inverts `visible_roi_px` to derive `preview_png_zoom` + `preview_png_pan_px`,
> CLAMPED to image bounds via `png_zoom_max` + the pan-slack clamp.
>
> **Badge floor holds:** `roi` rides the same routing as `preview` — badge-by-default,
> force-show only under `--urgent`/`--fe`. Aiming a viewport is *more* intrusive than a
> plain preview, so it obeys the never-yank rule (the 2026-07-10 semantics below).
>
> **Clamp-and-echo is ASYNC — NOT in the ack.** `fe.command.send` is fire-and-forget
> (§4): the daemon re-emits to all FEs and acks `{ok:true}` immediately with no FE
> round-trip, so the *effective* (clamped) rect is not knowable at ack time — the clamp
> is FE-side. The request-side ack therefore confirms only that the rect was forwarded.
> A caller that needs to detect heavy clamping and re-aim reads the effective rect from
> an **FE-emitted follow-up event** — proposed `preview_roi_applied{ws, path,
> effective:{x,y,w,h,src_w,src_h}, clamped:bool}` — not this ack. The request side ships
> first and DEGRADES GRACEFULLY: an FE without the solve ignores `roi` and previews
> normally. (FE solve + the `roi_applied` event + an optional `sot-fe … --await-roi`
> consumer are the follow-up halves.)
>
> **FE half SHIPPED (2026-07-21, same day).** The solve inverts `visible_roi_px` as a
> pure fn (`solve_roi_view`): rect-fitting zoom per axis, smaller axis wins so the whole
> rect stays visible, clamped to `[1, png_zoom_max]` *before* the pan solve (pan is in
> canvas px); pan centres the rect and the render pass's existing pan-slack clamp
> applies. The aim rides preview's routing untouched (badge floor holds; cross-ws aims
> fire at badge-consume) and is consumed by the render pass only once the aimed image is
> the *installed* quad — certified at preview-reply install, so it can never solve
> against the previous file's texture. The echo channel is the existing `agent.send`
> relay (the daemon re-broadcasts it to every connection as an `agent.message` evt —
> no protocol/daemon change), `text` being the event JSON:
> `{"evt":"preview_roi_applied", "ws", "path", "requested":{x,y,w,h},
> "effective":{x,y,w,h,src_w,src_h}, "clamped"}`. `clamped` = the requested rect is not
> fully inside the effective one (±2 px quantization slop): true only when the aim hit
> the zoom ceiling or ran off the image — the re-aim signal. `roi` is honoured on
> `reveal` too (the CLI attaches it to either verb). Still open: the `--await-roi`
> consumer.

> **Update — 2026-07-10 (PM revision): badge floor RESTORED as the default;
> "always show" means a COMPLETE show, not a stolen session.**
> Two rulings landed the same day. Morning: "show image should always set the
> nav to that file and show in nav pane; those should not be separate
> possibilities" — implemented as unconditional switch+show. After living with
> it, the maintainer clarified the intent: *"always set the nav and show means
> the file should be selected in the nav and shown in preview, NOT to yank my
> session over... I don't want to be yanked mid sentence."* The morning's real
> defect was **completeness** — the on-switch badge consume historically did
> not land the nav cursor (the #4 reveal gap) — not the badge model itself.
>
> Final semantics for `preview`/`reveal`:
> - **Same workspace** → render in place immediately (cursor + preview).
> - **Other workspace** → badge the row; the user is never force-switched.
>   On the user's next switch to that workspace the consume is COMPLETE:
>   nav cursor lands on the file AND the preview renders (the #4 reveal fix
>   in `switch_to_workspace` covers both first-visit and snapshot-revisit).
> - **`--urgent --fe <handle>`** = the explicit, user-requested focus-capture
>   option (no idle gate); route-layer still strips broadcast `urgent`, so a
>   blanket send can never yank every FE.
> `notify` keeps the status-line surface with a 10s sticky until the toast
> lands (ops TODO). Other fe-commands' semantics are unchanged.
**Supersedes:** ADR-0023 §4 (active-workspace gate on `preview`/`preview_image`). Keeps 0023's `op::FE_COMMAND` op, single-sink `dispatch_fe_command`, and the `devenv-fe` BE CLI. **Does NOT supersede the daemon boot-pty — its current, working design is ADR 0023's 2026-06-26 top Update (the original §3 proposal text is itself superseded). See the correction below.**

> **Correction — 2026-06-26: the boot-pty premise in this ADR (Context §2 and §6) is WRONG.**
> The daemon-side boot-pty (ADR 0023 §3) **does** boot claude. The "claude exits cleanly at
> init" that was blamed on a non-responsive terminal was actually `ccb`/`ccbe` invoking claude
> **bare** (`exec claude`): a daemon-spawned tmux pane inherits the tmux **server** env, not a
> login shell, so its PATH lacks `~/.local/bin` → `claude: not found` → the pane dies in <1s.
> It was invisible because the boot-pty discards claude's output. Fixed in `bdce722` (full-path
> claude + `export PATH` in ccb *and* ccbe). Verified: nav-pane create, comm-spawn, and the
> original `mypackage` case all boot claude in ~1s.
>
> Consequences: **the daemon boot-pty is NOT superseded — it is the canonical, working boot** (its
> current design is ADR 0023's 2026-06-26 Update; now firing for *every* `autostart_claude` create,
> gate `autostart_claude||boot`, commit `41eab5d`). **§6 below (daemon-hosted responsive terminal) is
> NOT required for boot — it is demoted to an OPTIONAL future enhancement, not a forced
> replacement of §3.** Context §1 (the gate is the flaw) and the imperative-commands decision STAND.

## Context

ADR-0023 was accepted, but its two load-bearing mechanisms were *believed* wrong in practice. **[Superseded 2026-06-26 — see the top correction: only the §4 GATE (item 1) was a real flaw. The daemon boot-pty (item 2 / 0023 §3) did NOT prove wrong; the ccb-PATH bug masked a working boot-pty. Read item 2 below as a retracted premise.]**

1. **The active-workspace GATE (0023 §4) is the flaw, not the model.** 0023 kept
   nav.preview's rule — *"only the FE currently viewing that workspace acts"* — for
   `preview`/`preview_image`. In practice that means a backend session's command is
   **silently dropped** whenever the user's FE is looking elsewhere. Live exhibit
   (2026-06-22): the backend pushed a result PNG to the preview pane; the FE had
   momentarily flipped workspaces (`myanalysis→mypackage→myanalysis` within ~1s); the
   push vanished with no feedback; **three sessions** (a peer session, win-fe, me) burned a
   debugging goose-chase before the daemon log showed the FE had simply been
   off-workspace. Connection fine, path fine, file fine — the *gate* ate it. A
   capability whose whole job is "show the user a result" must not depend on where the user
   already happens to be looking.

2. **The daemon-side boot-pty (0023 §3) does not boot claude.** **[RETRACTED PREMISE 2026-06-26 — FALSE: the boot-pty DOES boot claude; the real failure was ccb's bare `exec claude` off the daemon pane's PATH, fixed `bdce722` (see top correction). The paragraph below is the original wrong diagnosis, kept for history.]** 0023 assumed a
   server-internal pty that drains until `pane_current_command == claude` then closes
   would leave claude running. Empirically it does not: claude's TUI needs a
   **responsive** terminal at init, and a drain-only boot-pty is not one — claude exits
   cleanly at init (the same failure 0023's own Context notes for detached panes). The
   `feat/adr-0023-daemon-boot` branch carries the failed attempt.

Both failures share one root: **the FE's current view is treated as authority.** A
backend capability — show a result; boot a session — is gated on "is a FE looking at
the right place," when it has no business depending on that.

## Decision

**Principle: the daemon is authoritative and drives the FE as a viewport.** The daemon
owns the truth — which sessions exist, their state, what to display — and the FE renders
what the daemon tells it and forwards input. State that should not depend on "where
the user is looking" lives in the daemon, and the daemon *drives* the FE to it.

### 1. Commands are IMPERATIVE, not gated (reverses 0023 §4)

`op::FE_COMMAND` commands the FE to switch to a workspace and show a result *regardless
of the FE's current view*, instead of being silently dropped when the FE is elsewhere.
The active-workspace gate is removed from the preview path. The cross-workspace
limitation is gone with it: a session names any `{ws, path}` explicitly and the daemon
drives the FE there, so a result no longer has to live in the sender's own workspace
(a peer session's `example-project` figure becomes directly addressable).

### 2. Consent / notify model (imperative ≠ hijack) — FE-owned

- **Default = request-and-badge.** The command badges the target workspace's
  session-strip row (a "result pending" accent on the existing strip color/flash
  machinery) + a toast; **one-key jump, no switch.** Nothing is silently dropped — this
  is the floor, and it always reaches the user.
- **Force-show = `urgent:true`, and even then FE-gated.** Immediate switch+show, BUT if
  the drawer is focused and the current session is active, the FE **downgrades
  urgent→badge** — never eats a keystroke or yanks a live session. Force-show fires only
  when the FE is idle, and leaves a **jump-back breadcrumb** afterward.

### 3. Command vocabulary (`op::FE_COMMAND {v:1, cmd, args}`)

- `preview{ws, path, roi?}` — **MIME-agnostic**: markdown, source, AND images (backend
  `preview.get` mime drives md-vs-png; `png.rs` already renders images). Shows in the
  preview pane. No separate `preview_image` (redundant). A non-file/generated image (a
  temp plot not in the tree) is a deferred inline-bytes form of `preview`; v1 results
  are file-based per the `dev/output`|external-storage convention. **Optional `roi =
  {x,y,w,h}`** (source-image px, ADR-0022 `image.crop` vocabulary) aims the viewport at
  that region; FE clamps to bounds; effective rect echoes via a follow-up event, not the
  ack — see the 2026-07-21 top Update.
- `reveal{ws, path}` — switch + Files + expand the tree to `path` + **select the row**
  (deep cursor-reveal). "Go look at this"; distinct from `preview` (render it).
- `goto_workspace{ws}` = existing `FeCommand::Workspace{slug}`.
- `goto_mode{mode}` = existing `FeCommand::Mode{mode}`.
- `notify{text, level, ws}` = existing `FeCommand::Notify` (the badge primitive §2 uses).
- `spawn{label, project_root, agent_name, task?}` — create + boot a session. Full
  claude-boot works via the boot-pty (ADR 0023's 2026-06-26 Update) + the ccb PATH fix (`bdce722`), NOT
  the §6 terminal mechanism.
- `open_url{…}` — reserved (ADR-0024 backend web pages / TODO#4).

All arrive on the FE's existing `IncomingEvt::Event{op,payload}` arm, parse into an
`FeCommand`, and dispatch through the single `dispatch_fe_command` sink.

### 4. Transport (confirmed)

`op::FE_COMMAND` is a **daemon JSON-line-protocol op** (BE→daemon→FE), NOT the relay
envelope. That is what makes it **directed** (route to a specific/primary FE),
**imperative** (not a gated broadcast), and **ackable** (the FE acks e.g. "workspace
created + claude up" for `spawn`). BE→daemon send is the thin `devenv-fe` CLI over the
`comm-spawn.sh` daemon-socket endpoint — no comm relay, no FE LLM. **Back-compat:** the
relay `nav.preview` envelope STAYS (gated, workspace-scoped) so `sot-nav.sh` keeps
working; `op::FE_COMMAND` is the new imperative path.

### 5. Multi-FE routing (post-gate)

Removing the active-workspace gate reopens "which FE acts." Split by disruptiveness:

- **Badges broadcast to ALL FEs** — harmless, non-disruptive, no routing needed.
- **Force-show targets ONE FE = the primary/active one.** The daemon tracks a **primary
  FE**: the FE emits a lightweight `fe.active` signal (on window-focus + debounced
  keystroke); the daemon marks the most-recent as primary; force-show routes there only.
- **Sequencing:** **v1** ships the imperative channel with **badge-broadcast** (the core
  silent-drop fix) + force-show to an **explicit `--fe <handle>`** target. **v1.1** adds
  daemon primary-tracking (`fe.active` op + a `primary_client` field) so force-show
  auto-routes to "the FE I'm using." Small on both sides, the right UX, but off the v1
  critical path.

### 6. Boot = daemon-hosted responsive terminal + FE-as-viewport (OPTIONAL future — see correction at top)

> **Superseded premise (2026-06-26):** boot is SOLVED by the boot-pty (ADR 0023's 2026-06-26 Update) + the
> ccb PATH fix (`bdce722`) — claude boots in ~1s. This section's rationale ("a drain-only
> boot-pty can't boot claude") was wrong: the real failure was `ccb` calling `claude` bare
> while the daemon pane's PATH lacked `~/.local/bin`. The daemon-hosted responsive terminal
> below is therefore an OPTIONAL future UX/architecture enhancement, NOT a required boot
> mechanism and NOT a planned reversal of §3.

*(Archived proposal — the premise below is the retracted "boot-pty can't boot claude" diagnosis; boot already works via the boot-pty, see the note above. Kept as the optional future-viewport vision.)* This proposal assumed claude needs a responsive terminal at init that a drain-only boot-pty couldn't provide, so the daemon **would** host a **real terminal** for a booted session, with the FE attaching a **viewport** onto it. Same FE-as-viewport principle as §1: the daemon would own the terminal, the FE a window onto it. A session would boot daemon-side (no FE-nav) because the daemon — not the FE-attach — would provide the terminal. The mechanism
(daemon-owned terminal emulator vs. a daemon-owned persistent tmux *client* the FE
mirrors) is the deeper open engineering question (Open); it can land after the
command-channel half (§1–§5).

## Labor split (on `feat/op-fe-command`)

- **BE (daemon):** the `op::FE_COMMAND` protocol op, daemon emit + routing
  (v1 badge-broadcast + `--fe` target; v1.1 primary-tracking), the `devenv-fe` CLI.
- **win-fe (FE):** the `op::FE_COMMAND` handler (parse → `FeCommand` → existing
  `dispatch_fe_command`), the `preview`/`reveal` switch+show, the consent/notify UX (§2),
  and the `fe.active` signal (v1.1).

## Consequences

- A backend session can put a result in front of the user without their FE being
  pre-positioned, and from any workspace — the goose-chase failure class is gone.
- nav.preview's silent-drop is replaced by always-reaches-the user (badge floor) + opt-in,
  FE-gated force-show.
- Spawn no longer needs FE-nav — this works **today** via the daemon boot-pty + the ccb fix (see ADR 0023's 2026-06-26 Update); the §6 "daemon hosts the terminal" form is the optional future-viewport version, not how it boots now.
- One unifying principle — *daemon drives, FE views* — behind both the command channel
  and boot. The FE stops being a source of truth and becomes a viewport.

## Open / deferred

- **§6 boot terminal mechanism (OPTIONAL — boot already works via the boot-pty; see the
  §6 note)** — daemon-hosted terminal emulator vs. daemon-owned persistent tmux client the
  FE mirrors. A UX/architecture nice-to-have only; no longer needed for boot.
- **v1.1 primary-FE tracking** — `fe.active` op + daemon `primary_client`; fast-follow.
- **`preview` inline-bytes form** — for generated images not written to a file; deferred.
