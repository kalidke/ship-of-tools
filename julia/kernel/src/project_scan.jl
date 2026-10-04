# project.scan op: reads a project's sources statically into its module tree of definitions.

"""
    handle_project_scan

Walk the project's package source tree and return a nested
`modules → types/functions/submodules` structure. Drives the unified
Modules+Types navigation mode on the frontend.

Procedure:
1. Read `<project_root>/Project.toml` to find the package `name`.
2. Open `<project_root>/src/<name>.jl` (Julia package convention).
3. Recursively follow every `include("...")` call from that file.
4. For each file, run `collect_definitions` (the existing parser), then
   aggregate definitions into a module hierarchy: each module's children
   are types, functions, and submodules; functions whose name matches a
   sibling type's name are moved into that type's `:constructors` list.

Limitations of the v1:
- Constructors are detected by name-match only (inner/outer
  constructors merged); no signature collation yet.
- Per-function methods are still emitted as one entry per textual
  definition rather than grouped under a single function row; method
  collation is the next nav-system step.
- Parametric supertypes are returned as their full source text
  (`Bar{T}`); hierarchy grouping that strips parameters is the
  follow-up frontend step.

Wire shape (req has no fields):
```
res payload:
  project_root, package_name, entry_file,
  modules: [
    { name, file, line, ast_hash,
      types: [{ name, kind, file, line, ast_hash, constructors: [...],
                fields?: [{name, type, line}],   # struct only
                supertype?: "Bar" | "Bar{T}" }], # struct + abstract
      functions: [{ name, file, line, ast_hash }],
      submodules: [ ... same shape ... ] },
    ...
  ]
```
"""
function handle_project_scan(io::IO, state::KernelState, id, _payload)
    package_name = read_package_name(state.project_root)
    if isnothing(package_name)
        write_envelope(io, "res", id, "project.scan", Dict(
            :error => "no [name] in $(joinpath(state.project_root, "Project.toml"))",
            :code => "no_package",
        ))
        return
    end
    entry_path = joinpath(state.project_root, "src", "$(package_name).jl")
    if !isfile(entry_path)
        write_envelope(io, "res", id, "project.scan", Dict(
            :package_name => package_name,
            :entry_file => entry_path,
            :error => "missing entry file: $(entry_path)",
            :code => "no_entry",
        ))
        return
    end

    # Single context-carrying walk from the entry file: descend into module
    # bodies and follow include()s, attributing each def to the module it runs
    # in at runtime (see `scan_project_defs`). Group by physical file for the
    # tree builder, which keys `preview.get` off `:file`.
    defs = scan_project_defs(entry_path)
    file_defs = Dict{String, Vector{Dict}}()
    for d in defs
        push!(get!(() -> Dict[], file_defs, d[:file]), d)
    end

    modules = build_module_tree(file_defs, entry_path, package_name)

    write_envelope(io, "res", id, "project.scan", Dict(
        :project_root => state.project_root,
        :package_name => package_name,
        :entry_file => entry_path,
        :modules => modules,
    ))
end

"""
    read_package_name(project_root) -> String | nothing

Minimal Project.toml parser. Returns the value of the top-level
`name = "..."` key (the standard `[deps]`-free first section), or
`nothing` if the file doesn't exist or has no `name`. We don't pull in
TOML.jl here — Project.toml's package-name surface is consistently
quoted and lives before any `[section]` header, so a hand-roll keeps
the dep graph minimal.
"""
function read_package_name(project_root::AbstractString)
    p = joinpath(project_root, "Project.toml")
    isfile(p) || return nothing
    for raw in eachline(p)
        line = strip(split(raw, '#'; limit = 2)[1])
        isempty(line) && continue
        startswith(line, '[') && break  # entered a section; package name lives above
        if startswith(line, "name")
            kv = split(line, '='; limit = 2)
            length(kv) == 2 || continue
            val = strip(kv[2])
            val = strip(val, ['"', '\''])
            isempty(val) || return val
        end
    end
    return nothing
end

"""
    include_target(node) -> String | nothing

If `node` is an `include("literal")` call, return the literal path string;
otherwise `nothing`. Only the single-string-argument form is matched — the
static, statically-resolvable case the module nav cares about.
"""
function include_target(node)
    JuliaSyntax.kind(node) == K"call" || return nothing
    kids = JuliaSyntax.children(node)
    (isnothing(kids) || length(kids) != 2) && return nothing
    head, arg = kids[1], kids[2]
    (JuliaSyntax.kind(head) == K"Identifier" &&
     string(JuliaSyntax.sourcetext(head)) == "include" &&
     JuliaSyntax.kind(arg) == K"string") || return nothing
    return string_literal_text(arg)
end

"""
    scan_project_defs(entry_path) -> Vector{Dict}

Single recursive AST walk for `project.scan`. Starting at `entry_path`, it
descends into `module` bodies AND follows `include("…")` calls, carrying the
*enclosing module* name so a definition attributes to the module it runs in at
runtime — regardless of which physical file it sits in.

This replaces the old two-phase scan (flatten the include graph into a flat
file set → parse each file independently), which lost the enclosing-module
context: a file include()d inside `module Zernike` was parsed standalone, its
top-level defs got no `:parent`, and `build_module_tree` then defaulted them to
the package — so `Zernike` came up empty and its members listed flat under the
package. Carrying the module context across the include boundary fixes that.

Each entry has the `collect_definitions` shape (`:name`/`:kind`/`:line`/
`:ast_hash`, struct `:fields`/`:supertype`) plus a `:file` (the physical file)
and, for non-top-level defs, a `:parent` (the enclosing module name).
`build_module_tree` nests by `:parent`; it is correct as long as `:parent` is.
"""
function scan_project_defs(entry_path::AbstractString)
    defs = Dict[]
    visited = Set{String}()   # cycle guard: a file include-ing back into its includer
    scan_file_defs!(defs, abspath(entry_path), nothing, visited)
    return defs
end

# Parse one file and walk its top level in the scope of `current_module` — the
# module the `include` that pulled this file in was sitting in, or `nothing` for
# the entry file's true top level. `include` paths resolve relative to the
# current file (the standard Julia rule).
function scan_file_defs!(defs, path::AbstractString, current_module, visited::Set{String})
    abs_path = abspath(path)
    abs_path in visited && return
    isfile(abs_path) || return
    push!(visited, abs_path)
    src = try
        read(abs_path, String)
    catch
        return
    end
    tree = try
        JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, src; filename = abs_path)
    catch
        return
    end
    scan_block_defs!(defs, tree, current_module, dirname(abs_path), abs_path, visited)
end

# Walk the statements of `node` (a file root or a module/begin block),
# attributing each definition to `current_module`. Recurses into nested
# `module` bodies (with that module as the new scope) and follows `include`
# calls in the current scope; `base_dir`/`file` track the current file for
# include resolution and `:file` stamping.
function scan_block_defs!(defs, node, current_module, base_dir::AbstractString,
                          file::AbstractString, visited::Set{String})
    children = JuliaSyntax.children(node)
    isnothing(children) && return
    for child in children
        inc = include_target(child)
        if !isnothing(inc)
            scan_file_defs!(defs, abspath(joinpath(base_dir, inc)), current_module, visited)
            continue
        end
        entry = def_entry(child, JuliaSyntax.kind(child), current_module)
        if !isnothing(entry)
            entry[:file] = file
            push!(defs, entry)
            if entry[:kind] == "module"
                # Descend into the module body with this module as the new
                # enclosing scope — to any depth, across include()s.
                # `module_body_node` unwraps a docstring wrapper so a documented
                # `module M … end` (K"doc"[string, module]) is descended too.
                mnode = module_body_node(child, JuliaSyntax.kind(child))
                if !isnothing(mnode)
                    for mk in JuliaSyntax.children(mnode)
                        if JuliaSyntax.kind(mk) == K"block"
                            scan_block_defs!(defs, mk, entry[:name], base_dir, file, visited)
                        end
                    end
                end
            end
            continue
        end
        # Scope-transparent grouping (`begin … end`, parse toplevel): descend
        # with the SAME module so grouped includes/defs still attribute right.
        # (Conditional includes — `@static if … include … end` — are not
        # followed; a documented limitation, rare in package entry points.)
        k = JuliaSyntax.kind(child)
        if k == K"block" || k == K"toplevel"
            scan_block_defs!(defs, child, current_module, base_dir, file, visited)
        end
    end
end

function string_literal_text(node)
    kids = JuliaSyntax.children(node)
    isnothing(kids) && return nothing
    isempty(kids) && return nothing
    for c in kids
        if JuliaSyntax.kind(c) == K"String"
            return string(JuliaSyntax.sourcetext(c))
        end
    end
    return nothing
end

"""
    build_module_tree(file_defs, entry_path) -> Vector{Dict}

Aggregate per-file definitions into the nested `modules` shape the
frontend renders. Each entry in `file_defs[file]` carries `:kind` and
optional `:parent` (the enclosing module's name, set by
`collect_definitions` when recursing into a module block). Modules
themselves appear with `:kind == "module"` and no `:parent` for
top-level modules.

Today every entry's `:parent` field is the *immediate* enclosing
module name (or nothing for top-level modules in the entry file).
Multiple module names with the same string across files are *merged*
into a single module entry — sufficient while the v1 doesn't handle
collisions (a project that re-uses a name across unrelated submodules
is pathological). Future revisions can disambiguate by parent chain.
"""
function build_module_tree(file_defs::Dict{String, Vector{Dict}},
                           entry_path::AbstractString,
                           package_name::AbstractString)
    # Flat list of (entry, file) tuples for easier indexing.
    all_entries = Tuple{Dict, String}[]
    for (f, defs) in file_defs
        for d in defs
            push!(all_entries, (d, f))
        end
    end

    # Top-level entries in `include`d files have no `:parent` — they
    # run in the *includer*'s module scope at runtime, not at file
    # scope. Default them to the package module so `src/inner.jl`'s
    # `struct Inner` lands under `MyPkg`. Skip the package's own
    # module declaration (it really is top-level).
    for (d, f) in all_entries
        haskey(d, :parent) && continue
        if d[:kind] == "module" && d[:name] == package_name
            continue
        end
        d[:parent] = package_name
    end

    # Collect every module declaration as a candidate node. Top-level
    # modules are those with no :parent; submodules have :parent pointing
    # to their enclosing module's name.
    module_decls = Dict{String, Dict}()
    for (d, f) in all_entries
        if d[:kind] == "module"
            name = d[:name]
            # First-wins on collision so the entry file's module beats
            # an accidental duplicate in an included file.
            if !haskey(module_decls, name)
                module_decls[name] = Dict(
                    :name => name,
                    :file => f,
                    :line => d[:line],
                    :ast_hash => d[:ast_hash],
                    :types => Dict[],
                    :functions => Dict[],
                    :submodules => Vector{Dict}(),
                )
            end
        end
    end

    # Drop non-module entries into the correct module bucket.
    for (d, f) in all_entries
        d[:kind] == "module" && continue
        parent = get(d, :parent, nothing)
        isnothing(parent) && continue
        mod = get(module_decls, parent, nothing)
        isnothing(mod) && continue
        entry_dict = Dict(
            :name => d[:name],
            :kind => d[:kind],
            :file => f,
            :line => d[:line],
            :ast_hash => d[:ast_hash],
        )
        if d[:kind] == "struct" || d[:kind] == "abstract"
            entry_dict[:constructors] = Dict[]
            if haskey(d, :fields)
                entry_dict[:fields] = d[:fields]
            end
            if haskey(d, :supertype)
                entry_dict[:supertype] = d[:supertype]
            end
            push!(mod[:types], entry_dict)
        else
            push!(mod[:functions], entry_dict)
        end
    end

    # Move constructors (functions whose name matches a sibling type's
    # name in the same module) from :functions into the type's
    # :constructors list. Outer + inner constructors land here uniformly
    # because both are exposed as functions named `Foo` in the same
    # module's definition set.
    for (_, mod) in module_decls
        type_names = Set(t[:name] for t in mod[:types])
        kept_fns = Dict[]
        for fn in mod[:functions]
            if fn[:name] in type_names
                # Find the matching type and push.
                for t in mod[:types]
                    if t[:name] == fn[:name]
                        push!(t[:constructors], fn)
                        break
                    end
                end
            else
                push!(kept_fns, fn)
            end
        end
        mod[:functions] = kept_fns
        # Sort siblings alphabetically for predictable display.
        sort!(mod[:types]; by = t -> t[:name])
        sort!(mod[:functions]; by = f -> f[:name])
    end

    # Nest submodules under their parent. We do this *after* the flat
    # population so a submodule's own types/functions are already
    # attached to its node when we move it into its parent's :submodules.
    nested_names = Set{String}()
    for (name, mod) in module_decls
        # Find the module's own parent (look up its entry in all_entries).
        parent = nothing
        for (d, _) in all_entries
            if d[:kind] == "module" && d[:name] == name
                parent = get(d, :parent, nothing)
                break
            end
        end
        if !isnothing(parent) && haskey(module_decls, parent)
            push!(module_decls[parent][:submodules], mod)
            push!(nested_names, name)
        end
    end

    # Top-level modules = anything not nested under another.
    top = Dict[]
    for (name, mod) in module_decls
        name in nested_names && continue
        push!(top, mod)
    end
    sort!(top; by = m -> m[:name])
    top
end
