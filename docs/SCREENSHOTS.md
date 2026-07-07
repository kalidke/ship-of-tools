# Docs screenshots

The committed images under `docs/src/assets/screenshots/` are generated from
fixtures, not from a maintainer's private workspace. Regenerate them whenever UI
changes make the published docs visually stale.

## Scripted captures

```bash
scripts/docs-shots.sh run                         # all screenshots
scripts/docs-shots.sh run nav-files preview-math  # a subset
scripts/docs-shots.sh list                        # exact per-shot invocations
```

`run` builds the binaries if needed, stamps the fixture's fresh `.concept`
annotation (`sync-fixture`), boots a scratch `sotd` per project root, and drives
the frontend `--capture` harness. On a box without bash, use `list` output: each
shot is one `sotd` line plus one `sot` line that can be translated to the local
shell.

Capture-host dependencies beyond the normal dev stack:

- **MathJax sidecar**: `npm ci` in `rust/backend/sidecars/mathjax`. Without
  `node_modules`, math previews show raw LaTeX. `run` does this automatically.
- **poppler** (`pdftoppm` and `pdfinfo`) on PATH for the PDF preview shot.
- **Warm the stack before keeping shots**: the kernel is lazy-spawned and the
  MathJax sidecar boots async. `run` fires a discarded warm-up capture per
  daemon; manual runs should do the same. Keep per-shot delays >= 12 s.

## Conventions

Use the committed fixture workspace `docs/fixtures/DemoProject/` plus
`examples/preview/` for file-preview shots. Do not capture personal workspaces.

Use the same visual setup across shots: fullscreen, `--contrast-mode bright`,
dark theme, and no private sessions connected. If you edit fixture source, rerun
`scripts/docs-shots.sh sync-fixture`; leave `route_length.md`'s zero hash alone,
because it is deliberately stale for the drift-badge shot.

The Terminal drawer is documented in prose on the
[Terminal pane](src/guide/panes/terminal.md) page.

After replacing PNGs, run:

```bash
oxipng -o 4 --strip safe docs/src/assets/screenshots/*.png
```

Then rebuild the docs locally and inspect every page that embeds a changed shot.
