---
name: sot-fe-session-start
description: Alias of /sot-session-start for a Ship of Tools frontend session — the bootstrap already detects Windows and uses the FE inbox, local tunnel endpoint, and no-bridge rule. Activates for "fe session start", "frontend session start", "rearm fe comm".
---

Run `/sot-session-start` — it already branches on Windows (the FE inbox as
the watch source, no relay bridge, selftest over the local tunnel). Kept
only until the FE driver's drawer cutover retires this name.
