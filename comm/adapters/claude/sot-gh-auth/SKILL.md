---
name: sot-gh-auth
description: Authenticate the GitHub CLI on a headless box — drives the OAuth device flow so you enter a code in any browser and gh finishes by polling. Use when gh auth is broken or git push fails with an auth error. Activates for "gh auth", "gh login".
---

# sot-gh-auth

One deterministic script (`~/.sot-comm/bin/sot-gh-auth.sh`, installed by
`ShipTools.update_comm()`), plus a bare-command wrapper `sot-gh-auth` in
`~/.local/bin` — tell users that form. On a headless box, `gh auth login`'s
web flow wedges on a text browser and hides the code; the device flow needs
no local browser — you enter a short code in *any* browser and gh finishes
by polling. Storage is forced to `~/.config/gh/hosts.yml` (0600,
`--insecure-storage`) so one token covers every host sharing that home dir.

## How to run it (Claude-driven)

Split so the code reaches the user before the blocking poll:

1. **Request a code**: `sot-gh-auth.sh request`. Exit 0 with "already
   authenticated" → report and stop. Exit 4 (`GH_TOKEN`/`GITHUB_TOKEN` set,
   which overrides hosts.yml) → tell the user to unset it and retry.
   Otherwise prints `SOT_GH_USER_CODE=…`, `SOT_GH_VERIFY_URL=…`,
   `SOT_GH_EXPIRES_IN=…`.
2. **Surface the code to the user**: "Open https://github.com/login/device
   and enter `XXXX-XXXX` (any browser). Expires in ~15 min." Relay it to an
   attached frontend too if one exists: `comm-relay.sh send @win-fe "..."`.
3. **Poll to completion**: `sot-gh-auth.sh poll` (blocks until authorized,
   runs `gh auth setup-git`, prints `gh auth status`). Report the
   authenticated login + scopes.

A human at a terminal can skip the split: bare `sot-gh-auth` runs the full
flow (request → print code → poll).

## Subcommands

| command | does |
|---------|------|
| `sot-gh-auth.sh` | full flow, for humans in a terminal |
| `sot-gh-auth.sh request` | request + print a code, exit immediately |
| `sot-gh-auth.sh poll` | poll a saved request through to completion |
| `sot-gh-auth.sh status` | `gh auth status` — diagnostic only, not the flow gate |

## Guards

- Idempotent: `request`/full short-circuit if already authenticated
  (`gh auth logout` first to force a fresh login).
- Refuses if `GH_TOKEN`/`GITHUB_TOKEN` is set (re-auth would be a silent
  no-op otherwise).
- Needs `gh`, `curl`, `jq` on PATH — checked up front, exits 3 naming the
  missing one.
- Secrets never touch argv (token via stdin, device_code via a 0600 temp
  file); state + temp file are removed by an EXIT/INT/TERM trap.
- Env overrides: `GH_HOST`, `SOT_GH_SCOPES` (default
  `repo read:org gist workflow`), `GH_OAUTH_CLIENT_ID` (default gh's public
  client_id — the device flow is designed for a public client_id; the
  consent screen honestly reads "GitHub CLI").

## Revoke

`gh auth logout -h github.com` and revoke the "GitHub CLI" authorization at
<https://github.com/settings/connections>. Drop scopes via `SOT_GH_SCOPES`
if you don't need them all (`repo` is required for private `git push`).
