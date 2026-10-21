# julia/kernel: the Julia kernel that reads sources and runs plugin previews (sidecars)

One Julia process per workspace, which the daemon runs with `--project=julia/kernel`. It reads project sources
statically (JuliaSyntax) and runs the FileType plugins' previews. It never runs user code and never loads the user's
environment. Part of the sidecars; charter: rust/backend/src/sidecars/CLAUDE.md (that page may not exist yet).

## Files
- `Project.toml`: the package, its plugin dependencies and `[sources]` paths.
- `src/ShipToolsKernel.jl`: the serve loop, `dispatch`, hello, `file.parse` and the envelope writer, plus the one op no reachable client path sends (function.methods).
- `src/definitions.jl`: top-level definitions of a parsed source with per-entity AST hashes.
- `src/preview.jl`: `file.preview` and the lazy loading of built-in plugins.
- `src/project_scan.jl`: `project.scan`, the static module tree of a project.
- `src/tokenize.jl`: `markdown.tokenize`, definition spans of a Julia source.
- `test/runtests.jl`: the kernel suite.
- `test/scan_nesting.jl`: checks that `project.scan` nests submodules and descends docstringed modules.
- `test/fixtures/`: sources the suite reads.

## Start here
`dispatch` in src/ShipToolsKernel.jl, for an op (an op lands with its sender); `handle_file_preview` in
src/preview.jl, for plugins.

## Rules
- One `res` per request id; a failure is `{error, code}` and the loop survives (`serve`).
- `PROTOCOL_VERSION` equals the daemon's `KERNEL_PROTOCOL_VERSION`.
- Wire paths are forward-slash and project-relative (`resolve_request_path`).
- The per-entity hash ignores whitespace, comments and docstrings (`definition_ast_hash`; tested).
- The `include` lines follow `KernelState`, because handler signatures name it.
- `write_envelope` has a twin in julia/repl.
- `[sources]` paths in Project.toml are relative to it.
