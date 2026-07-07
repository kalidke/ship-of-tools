# `r` / `R` run-file demo

A two-file pair that exercises the NavTree run-file keybinds (priority J).

| Key | Action |
|-----|--------|
| `r` | Reset the workspace REPL to a fresh `julia --project=<closest Project.toml>`, then `include()` the cursored file. |
| `R` | `include()` the cursored file into the **existing** REPL — no reset. |

## Try it

1. Cursor `01_setup.jl`, press **`r`**. The REPL bounces to a fresh session and
   prints its setup confirmation. It is self-contained, so a clean REPL is fine.
2. Cursor `02_use_setup.jl`, press **`R`**. It inherits `GREETING`, `samples`, and
   `summarize` from step 1 and extends the live session state.

Reverse it to see the dependency: press `r` on `02_use_setup.jl` and it throws
`UndefVarError` on `GREETING`, because the fresh REPL never ran `01_setup.jl`.
