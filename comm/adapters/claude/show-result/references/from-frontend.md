# show-result from a FRONTEND session

Everything in `SKILL.md` assumes a booted **backend** session. A Claude
running in the Windows FE Terminal drawer (`win-fe-<host>`, bootstrapped by
`/sot-fe-session-start`) has neither of the things the auto-discovery relies
on, and fails in a way that reads like a dead daemon:

```
ERROR: could not find the sotd daemon. Set --endpoint unix:/path or tcp:HOST:PORT (or $SOT_FE_ENDPOINT).
```

That message appears **even with the tunnel plainly working**. Two
similarly-named variables carry the same value to different tools, and only
one of them is set for you:

| Variable | Read by | Set by session-start bootstrap? |
|---|---|---|
| `SOT_RELAY_ENDPOINT` | `comm-relay.sh` (messaging) | yes |
| `SOT_FE_ENDPOINT` | `sot-fe` (`preview`/`docs`/`open-url`) | no — export it yourself |

Two fixes:

1. **Export `SOT_FE_ENDPOINT` yourself** — same value, the FE's local
   forwarded port:

   ```bash
   export SOT_FE_ENDPOINT="tcp:127.0.0.1:${SOT_PORT:-18743}"
   ```

2. **Pass the workspace slug explicitly** — there's no tmux on the FE box
   and `$SOT_WORKSPACE` is unset, so slug auto-discovery yields nothing:

   ```bash
   ~/.sot-comm/bin/sot-fe preview sot examples/preview/quarto_julia.qmd
   ```

The path stays workspace-relative and backend-side — it resolves against
the target workspace on the daemon's disk, not the FE machine's checkout,
even though you're typing it on the FE. Same two rules apply to `docs` and
`open-url`.

## Manual fallback on a backend session (if `show-result` isn't on PATH)

`show-result` (in `~/.local/bin`) auto-discovers your workspace slug — it's
what `sot-nav.sh` uses internally: `${SOT_WORKSPACE:-$(tmux
display-message -p '#S' | sed -n 's/^sot-be-//p')}`. If it's missing from
PATH, call the same thing directly:

```bash
WS=${SOT_WORKSPACE:-$(tmux display-message -p '#S' | sed -n 's/^sot-be-//p')}
~/.sot-comm/bin/sot-fe preview "$WS" "<path>"
```
