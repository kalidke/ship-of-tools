---
name: sot-be-session-start
description: Alias of /sot-session-start for a Ship of Tools backend session — the bootstrap already runs the sot layer (FE ping, bus sync) when it detects this repo. Activates for "be session start", "backend session start", "rearm be comm".
---

Run `/sot-session-start` — it already does everything this skill used to,
including the Ship of Tools layer, whenever `comm-session-start.sh` detects
this repo. Kept only so an older launcher naming `ccbe` still resolves.
`/sot-fe-session-start` is now a deprecated alias of this skill — there is
no separate frontend-driver session role (a frontend is a client, never a
comm peer).
