---
name: sot-fe-session-start
description: Deprecated alias of sot-be-session-start — there is no more frontend-driver session role (a frontend is a client, never a comm peer); a Windows session is a session like any other. Use for "fe session start", "frontend session start", "rearm fe comm".
---

# sot-fe-session-start

Deprecated: the frontend-driver session role this name once meant is
retired — a session's identity is its row's handle everywhere, Windows
included, and a frontend is addressed as `fe@<host>` by `sot-fe --fe <host>`,
never joined as. Run `sot-be-session-start`.
