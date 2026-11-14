Part of fe-ui: see ../CLAUDE.md.

The pane supplies the dial host to the capsule's attach diagnostics. A switch that drops an uncheckpointed or dead
client labels its shutdown `switch away`; `retire_leaving`, the one place the pane retires a client, names `warm pool
eviction`, `dead warm client`, `row removed` or `kept row invalidated` at the respective retirement point. These labels add log context only; parking, dropping and shutdown
keep their existing behavior.
