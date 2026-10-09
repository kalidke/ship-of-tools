# show-result from a FRONTEND session

Everything in `SKILL.md` assumes a booted **backend** session. A Claude
running in the Windows FE Terminal drawer is a session like any other
(bootstrapped the same as any Ship of Tools session — `fe@<host>` is the
frontend PROCESS's address, not this session's handle) and has neither of the things the
auto-discovery relies on, and fails in a way that reads like a dead daemon:

```
ERROR: could not find the sotd daemon. Set --endpoint unix:/path, ssh:target[/host], or (Windows) pipe:name (or $SOT_FE_ENDPOINT).
```

That message appears **even with the frontend's own connection plainly working**.
`sot-fe` (`preview`/`docs`/`open-url`) reads `SOT_FE_ENDPOINT`, and nothing sets
it for you; messaging needs no variable, because `comm-relay.sh` asks `sotd
topology relay-endpoint`.

Two fixes:

1. **Export `SOT_FE_ENDPOINT` yourself** — only `unix:`, `ssh:target[/host]`
   (Linux/macOS) or `pipe:name` (Windows) are dialable; an endpoint you
   name in any other form is refused, never silently replaced by this
   box's own daemon. On a remote box:

   ```bash
   export SOT_FE_ENDPOINT="ssh:<hub>"
   ```

2. **Pass the workspace slug explicitly** — `$SOT_WORKSPACE` is unset on the
   FE box, so slug auto-discovery yields nothing:

   ```bash
   ~/.sot-comm/bin/sot-fe preview sot examples/preview/quarto_julia.qmd
   ```

The path stays workspace-relative and backend-side — it resolves against
the target workspace on the daemon's disk, not the FE machine's checkout,
even though you're typing it on the FE. Same two rules apply to `docs` and
`open-url`.

## Manual fallback on a backend session (if `show-result` isn't on PATH)

`show-result` (in `~/.local/bin`) auto-discovers your workspace slug — it's
what `sot-nav.sh` uses internally: `$SOT_WORKSPACE`, stamped into the row's
env by the daemon. If it's missing from PATH, call the same thing directly:

```bash
WS=$SOT_WORKSPACE
~/.sot-comm/bin/sot-fe preview "$WS" "<path>"
```
