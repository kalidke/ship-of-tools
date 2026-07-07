---
name: sot-gh-auth
description: Authenticate (or re-authenticate) the GitHub CLI (gh) on a headless box without the browser-wedge — drives the OAuth device flow directly so you enter a short code in any browser (e.g. your Windows one) and gh completes by polling. Use when gh auth is broken/expired, `gh`/`git push` over HTTPS fails with an auth error, or you want to log gh in on a server. Activates for "gh auth", "github auth", "gh login", "authenticate gh", "gh token expired", "re-auth github", "fix gh auth", "gh auth broken", "/sot-gh-auth".
---

# sot-gh-auth

One deterministic script, installed by `ShipTools.update_comm()` from
`comm/core/scripts/sot-gh-auth.sh` to **`~/.sot-comm/bin/sot-gh-auth.sh`**,
plus a bare-command wrapper **`sot-gh-auth`** in `~/.local/bin` (on PATH) so a
human at any shell can just type `sot-gh-auth` — tell users THAT form.
Additionally `gh config set browser echo` is the fleet posture: even a raw
`gh auth login` now prints the URL instead of wedging a text browser.

## Why (the failure this fixes)

On a headless box `gh auth login`'s web flow runs `xdg-open` → a text
`www-browser`, which **wedges** on a cookie prompt, hides the one-time code, and
blocks the terminal — the login never completes and the expired token lingers in
`~/.config/gh/hosts.yml`. The device flow needs **no local browser**: you enter a
short code in *any* browser and gh on this box finishes by polling GitHub. This
script drives that flow directly (the same one gh uses internally), so there is
nothing to launch and nothing to wedge.

Storage is forced to `~/.config/gh/hosts.yml` (0600) via `--insecure-storage`, on
purpose: on an optional multi-host / shared-home deployment, one hosts.yml token
covers every host sharing that home directory — a per-machine keyring token would
not. Separate-filesystem machines authenticate separately.

## How to run it (Claude-driven — the /sot-gh-auth path)

Split so the code reaches the user **before** the blocking poll. Use full paths.

1. **Request a code (fast, non-blocking + the gate).** Run
   `~/.sot-comm/bin/sot-gh-auth.sh request`. It:
   - exits **0 with "already authenticated"** → report that and stop;
   - exits **4** (GH_TOKEN/GITHUB_TOKEN set) → tell the user to
     `unset GH_TOKEN GITHUB_TOKEN` and retry (that env token overrides hosts.yml);
   - otherwise prints machine-parseable lines up front:
     `SOT_GH_USER_CODE=…`, `SOT_GH_VERIFY_URL=…`, `SOT_GH_EXPIRES_IN=…`.
   (Prefer this over the `status` subcommand as the gate — `status` alone can be
   fooled by an env token into false-passing without repairing hosts.yml.)
2. **Surface the code prominently to the user** (in chat), e.g.:
   > Open **https://github.com/login/device** and enter **`XXXX-XXXX`** (any
   > browser — your Windows one is fine). Expires in ~15 min.
   If a frontend is attached, also relay it:
   `~/.sot-comm/bin/comm-relay.sh send @win-fe "[gh-auth] open https://github.com/login/device, code XXXX-XXXX"`.
3. **Poll to completion.** Run `~/.sot-comm/bin/sot-gh-auth.sh poll` (blocks until
   you authorize; auto-completes, runs `gh auth setup-git`, prints `gh auth
   status`). Report the authenticated login + scopes it prints.

Human running it directly in a terminal can skip the split and just run
`~/.sot-comm/bin/sot-gh-auth.sh` (full flow: prints the code, then polls in the
same call).

## Subcommands

| command | does |
|---------|------|
| `sot-gh-auth.sh`         | full flow (request → print code → poll). For humans in a terminal. |
| `sot-gh-auth.sh request` | request + print a code, save state, exit immediately. For the split flow. |
| `sot-gh-auth.sh poll`    | poll the saved request through to completion. |
| `sot-gh-auth.sh status`  | `gh auth status` (exit 0 iff a valid live token). Diagnostic only — not the flow gate. |

## Guards / notes

- **Idempotent**: `request`/`full` short-circuit if already authenticated (run
  `gh auth logout` first to force a fresh login).
- **Refuses if `GH_TOKEN`/`GITHUB_TOKEN` is set** — gh honors that env token over
  `hosts.yml`, so re-auth would be a silent no-op; unset it first.
- **Needs `gh`, `curl`, `jq`** on PATH; checked up front (incl. before issuing a
  device code) — exits 3 naming the missing one.
- **Secrets hygiene**: the access token is piped to gh via STDIN (never argv); the
  device_code goes to curl via a 0600 temp file (never argv → never in
  `/proc/PID/cmdline`). State (`~/.sot-comm/gh-device-auth.json`, 0600) + temp
  file are removed by an EXIT/INT/TERM trap in the polling path.
- Env overrides: `GH_HOST` (default `github.com`, e.g. a GHE host),
  `SOT_GH_SCOPES` (default `repo read:org gist workflow`),
  `GH_OAUTH_CLIENT_ID` (default gh's public github.com client_id).

## Blast radius / revoke

The token requests `repo read:org gist workflow` (matching what gh itself
requests) and is stored **plaintext** in `hosts.yml` (0600, but root-readable on
any host with access to that home directory — inherent to shared-home gh).
Drop scopes via `SOT_GH_SCOPES` if you don't need them (`repo` is required for
private `git push`). To revoke: `gh auth logout -h github.com` **and** revoke the
"GitHub CLI" authorization at <https://github.com/settings/connections>.

## Client-id note

The script reuses gh's own **public** OAuth client_id for the device flow (that is
what device flow is designed for; the consent screen honestly reads "GitHub CLI"
and the token is handed straight to gh). Fine for a personal cluster. A dedicated
OAuth App is the cleaner-but-heavier alternative if you ever want your own consent
identity; override with `GH_OAUTH_CLIENT_ID`.
