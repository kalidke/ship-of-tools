# agents/sot-fe: a session drives the frontend and its REPL over the daemon's wire (agents)

The CLI a session uses to drive the frontend, its REPL and its sibling rows: one verb per request, sent to the daemon,
never through the comm relay. Part of agents; charter: agents/CLAUDE.md. The installer copies the files of this folder
flat into the comm bin (`comm/bin-folders.txt`), next to `comm-lib.sh`, which they source.

## Files
- `sot-fe`: the entry: usage header, flags, the `source` lines of the four parts, and the verb dispatch
- `sot-fe-request.sh`: the daemon request (`resolve_endpoint`, `sot_send`), the `fe.command` verbs, workspace lookup
- `sot-fe-repl.sh`: the REPL verbs: run a file, eval, interrupt, status
- `sot-fe-row.sh`: type into a row, read its screen, move it to another account
- `sot-fe-version.sh`: the version report (`send_version`)
- `sot-nav.sh`: the nav envelope (`nav.preview`) broadcast with `comm-relay.sh send --all`

## Start here
The `case "$CMD"` dispatch at the foot of `sot-fe`, then the `send_*` function its verb calls, in the part of its
concept.

## Rules
- The usage text is the entry's header comment: `usage` prints it from `$0`, from line 10 on, so the header stays in
  the entry.
- Every request is one `sot_send`, which is one `sot_oneshot_request` (comm-lib.sh) that prepends the hello frame
  (`sot_hello_frame`).
- `--timeout` is validated once, at flag parse.
- Free text reaches jq through `sot_jq_rawfile`, never as an argument.
- An unknown flag is an error, never a positional.
- A part defines functions only and is sourced before the dispatch runs; a new verb's function goes in the part of its
  concept, and a new part gets a `source` line in the entry.
