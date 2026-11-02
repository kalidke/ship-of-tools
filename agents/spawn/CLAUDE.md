Part of agents: see ../CLAUDE.md.

`comm-probe.sh up` gives each create a fresh folder and label, and records the returned ID and endpoint under
`probe/<handle>/created-rows`. `down` ends only those recorded rows at their recorded endpoints through
`workspace.destroy`, including rows whose responder never joined. A reused row has
no new ownership record. A failed end is printed, kept for a later `down`, and never prevents the next row's end.
`agents/tests/test-comm-probe.sh` exercises creation, reuse, shutdown and a failed end through the production functions.
