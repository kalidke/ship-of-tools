# Definition extraction: top-level definitions of a parsed source with per-entity AST hashes.

# Walk a JuliaSyntax tree, collect top-level definitions as
# {name, kind, line, ast_hash, parent}. Descends one level into module
# bodies so a typical Julia file (which usually wraps its contents in
# `module Foo ... end`) surfaces the inner definitions Modules-mode wants
# to see. `ast_hash` is per-entity: walking the entity's SyntaxNode
# subtree, the hash is stable under whitespace/comment edits (SyntaxNode
# already skips trivia) and sensitive to any structural or value change.
# Per-entity hashing is what concept-annotation `synced_against` is keyed
# on per ADR 0005 / CLAUDE.md — a file-level hash would mark every
# annotation stale on any edit, defeating the reactive-staleness UX.
# Build a single definition entry Dict from a child node, or `nothing` if the
# child isn't a recognized definition. Shared by `collect_definitions`
# (file.parse) and the project-scan walk (`scan_block_defs!`) so the entry shape
# — name/kind/line/ast_hash, plus struct fields/supertype and an optional
# `:parent` — lives in one place.
function def_entry(child, k, parent_name)
    name, kind_str, hash_node = definition_for(child, k)
    name === nothing && return nothing
    entry = Dict(
        :name => name,
        :kind => kind_str,
        :line => JuliaSyntax.source_line(child),
        :ast_hash => definition_ast_hash(hash_node),
    )
    if parent_name !== nothing
        entry[:parent] = parent_name
    end
    # Type-specific enrichments — F5 (supertype edge) + F6 (struct fields).
    # Both come from the same SyntaxNode we'd otherwise throw away after
    # hashing; cheap to extract so the Modules/Types nav can drill into a
    # type without a follow-up wire call.
    if kind_str == "struct"
        fields = extract_struct_fields(hash_node)
        isempty(fields) || (entry[:fields] = fields)
        sup = extract_supertype(hash_node)
        sup === nothing || (entry[:supertype] = sup)
    elseif kind_str == "abstract"
        sup = extract_supertype(hash_node)
        sup === nothing || (entry[:supertype] = sup)
    end
    return entry
end

function collect_definitions(node, parent_name = nothing)
    defs = Dict[]
    children = JuliaSyntax.children(node)
    isnothing(children) && return defs
    for child in children
        k = JuliaSyntax.kind(child)
        entry = def_entry(child, k, parent_name)
        entry === nothing && continue
        push!(defs, entry)
        if entry[:kind] == "module" && parent_name === nothing
            # Walk the module body one level deep so members surface in a
            # single-file parse (file.parse). `module_body_node` unwraps a
            # docstring wrapper so a documented module's block is found.
            # Nested modules and include()d submodule bodies are handled by the
            # project-scan walk (`scan_block_defs!`), not here.
            mnode = module_body_node(child, k)
            if !isnothing(mnode)
                for mk in JuliaSyntax.children(mnode)
                    if JuliaSyntax.kind(mk) == K"block"
                        append!(defs, collect_definitions(mk, entry[:name]))
                    end
                end
            end
        end
    end
    defs
end

# Returns `(name, kind_str, hash_node)`. `hash_node` is the SyntaxNode the
# entity's `ast_hash` should be computed from — usually the def itself,
# except for `K"doc"`-wrapped definitions where we strip the docstring so
# docstring edits don't invalidate the hash. The docstring is its own
# annotation surface; per-entity hash should be the code-only fingerprint.
function definition_for(child, k)
    if k == K"function"
        return (def_name(child), "function", child)
    elseif k == K"struct"
        return (def_name(child), "struct", child)
    elseif k == K"abstract"
        return (def_name(child), "abstract", child)
    elseif k == K"module"
        return (def_name(child), "module", child)
    elseif k == K"macro"
        return (def_name(child), "macro", child)
    elseif k == K"="
        # function f(x) = ... style. The lhs is a call; the name is the
        # call's first arg.
        kids = JuliaSyntax.children(child)
        if !isnothing(kids) && length(kids) >= 1 && JuliaSyntax.kind(kids[1]) == K"call"
            return (def_name(kids[1]), "function", child)
        end
    elseif k == K"doc"
        # Docstring + definition pair. The actual definition is the second
        # child (the first is the docstring expression).
        kids = JuliaSyntax.children(child)
        if !isnothing(kids) && length(kids) >= 2
            name, kind_str, _ = definition_for(kids[2], JuliaSyntax.kind(kids[2]))
            # Hash the inner def, not the K"doc" wrapper — docstring edits
            # leave `ast_hash` unchanged.
            return (name, kind_str, kids[2])
        end
    end
    return (nothing, "", child)
end

# The actual `module` SyntaxNode for a child that is a module definition,
# unwrapping a K"doc" docstring wrapper if present (`"docs" module M … end`
# parses as K"doc"[string, module]). Returns `nothing` if `child` isn't a
# module def. Used to reach the module body's block for descent — without this,
# a docstringed module's members are never collected (the block sits inside the
# `module` node, not the `doc` wrapper we'd otherwise walk).
function module_body_node(child, k)
    if k == K"module"
        return child
    elseif k == K"doc"
        kids = JuliaSyntax.children(child)
        if !isnothing(kids) && length(kids) >= 2 && JuliaSyntax.kind(kids[2]) == K"module"
            return kids[2]
        end
    end
    return nothing
end

# Walk a struct's K"block" body, collect typed and untyped field
# declarations into `[{name, type, line}]`. Skips inner constructors
# (K"function" / K"call" defs) and any other non-field expressions a
# user might put inside a struct body. `type` is the verbatim source
# text of the type expression (`Vector{Int}`, `Tuple{Symbol, Any}`) or
# the empty string for an untyped field. Default-value field declarations
# (`x::Int = 0`) recurse into the LHS to extract name+type, dropping
# the default — the default value isn't part of the field signature
# the nav cares about.
function extract_struct_fields(struct_node)
    fields = Dict[]
    kids = JuliaSyntax.children(struct_node)
    isnothing(kids) && return fields
    for c in kids
        JuliaSyntax.kind(c) == K"block" || continue
        body_kids = JuliaSyntax.children(c)
        isnothing(body_kids) && continue
        for stmt in body_kids
            push_field_if_field!(fields, stmt)
        end
    end
    return fields
end

function push_field_if_field!(fields, node)
    k = JuliaSyntax.kind(node)
    if k == K"::"
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) == 2
            name_node, type_node = kids[1], kids[2]
            if JuliaSyntax.kind(name_node) == K"Identifier"
                push!(fields, Dict(
                    :name => string(JuliaSyntax.sourcetext(name_node)),
                    :type => string(JuliaSyntax.sourcetext(type_node)),
                    :line => JuliaSyntax.source_line(node),
                ))
            end
        end
    elseif k == K"Identifier"
        push!(fields, Dict(
            :name => string(JuliaSyntax.sourcetext(node)),
            :type => "",
            :line => JuliaSyntax.source_line(node),
        ))
    elseif k == K"="
        # `x::T = default` or `x = default` — pull name+type from LHS,
        # ignore the default expression on the RHS.
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) >= 1
            push_field_if_field!(fields, kids[1])
        end
    end
    # K"function" / K"call" inside a struct body is an inner
    # constructor; the constructor-merge pass in `build_module_tree`
    # already handles those, so we skip here.
end

# Pull the supertype identifier (text) out of a K"struct" / K"abstract"
# definition. Walks the top-level children for a K"<:" expression and
# returns the source text of its RHS. Handles parametric forms like
# `Foo{T} <: Bar` and `Foo{T} <: Bar{T}` — for the parametric case the
# returned text is the full `Bar{T}`, which the nav can use as-is or
# strip down to just `Bar` for hierarchy grouping. Returns `nothing`
# when the type has no explicit supertype (defaults to `Any`).
function extract_supertype(type_node)
    kids = JuliaSyntax.children(type_node)
    isnothing(kids) && return nothing
    for c in kids
        if JuliaSyntax.kind(c) == K"<:"
            sub_kids = JuliaSyntax.children(c)
            if !isnothing(sub_kids) && length(sub_kids) == 2
                return string(JuliaSyntax.sourcetext(sub_kids[2]))
            end
        end
    end
    return nothing
end

# Per-entity AST hash. SHA-256 of a deterministic kind+leaf-text walk
# of the SyntaxNode subtree. SyntaxNode already excludes trivia
# (whitespace/comments), so the hash is whitespace- and comment-stable
# but flips on any structural or value change. NUL bytes separate fields
# to keep the byte stream unambiguous across kind/text boundaries.
function definition_ast_hash(node)
    ctx = SHA.SHA2_256_CTX()
    walk_for_hash!(ctx, node)
    bytes2hex(SHA.digest!(ctx))
end

function walk_for_hash!(ctx, node)
    SHA.update!(ctx, codeunits(string(JuliaSyntax.kind(node))))
    SHA.update!(ctx, UInt8[0x00])
    kids = JuliaSyntax.children(node)
    if isnothing(kids) || isempty(kids)
        SHA.update!(ctx, codeunits(JuliaSyntax.sourcetext(node)))
        SHA.update!(ctx, UInt8[0x00])
    else
        for c in kids
            walk_for_hash!(ctx, c)
        end
    end
end

function def_name(node)
    kids = JuliaSyntax.children(node)
    isnothing(kids) && return nothing
    for c in kids
        ck = JuliaSyntax.kind(c)
        if ck == K"Identifier"
            return string(JuliaSyntax.sourcetext(c))
        elseif ck == K"call"
            return def_name(c)
        elseif ck == K"<:" || ck == K"curly"
            # `struct Foo <: Bar`, `struct Vec{T}`, or the combination —
            # JuliaSyntax wraps the name in a K"<:" / K"curly" node before
            # exposing the Identifier. Descend once; the first nested
            # Identifier (or further-wrapped Identifier) is the def name.
            n = def_name(c)
            if n !== nothing
                return n
            end
        end
    end
    return nothing
end
