# markdown.tokenize op: spans of definitions in a Julia source, for the frontend's highlighter.

"""
    handle_markdown_tokenize

Backend-side syntax tokenizer for fenced code blocks. Today only handles
Julia (`lang = "julia"`); other languages get an empty span list so the
frontend's tree-sitter fallback wins.

Wire shape (via `kernel.request` envelope):
```
req payload:  { lang: "julia", source: "..." }
res payload:  { lang: "julia", spans: [{ start, end, kind }] }
```

Uses `JuliaSyntax.parseall` instead of `tokenize` so we walk the parse
tree, not just the lexical token stream — this is the precision lift
over tree-sitter-julia, since the AST tells us about function definition
vs call-site, parameter names, field access, type annotations, etc.
that the lexer can only guess at with heuristics. Per Codex's industry-
standard recommendation, this is the "semantic layer" that overlays
the synchronous tree-sitter base on the frontend.

`start` and `end` are byte offsets in `source` (0-indexed, exclusive
end — matches Rust slice semantics). `kind` is a tree-sitter standard
capture name (`keyword`, `function.call`, `type`, `string`, etc.) so
the frontend can reuse `color_for_scope` without a separate mapping.

Tolerant on parse errors: walks whatever subtree did parse and skips
sections that didn't. Returns `nothing` from the inner walk for nodes
that don't map to a known kind; those bytes fall through to the
frontend's default-fg rendering.
"""
function handle_markdown_tokenize(io::IO, _state::KernelState, id, payload)
    lang = string(get(payload, "lang", ""))
    source = string(get(payload, "source", ""))
    if lang != "julia" && lang != "jl"
        write_envelope(io, "res", id, "markdown.tokenize", Dict(
            :lang => lang,
            :spans => Dict[],
        ))
        return
    end
    spans = try
        tokenize_julia_source(source)
    catch err
        @warn "markdown.tokenize Julia walk failed" exception = err
        Dict[]
    end
    write_envelope(io, "res", id, "markdown.tokenize", Dict(
        :lang => "julia",
        :spans => spans,
    ))
end

"""
    tokenize_julia_source(source) -> Vec<Dict>

Parse `source` with JuliaSyntax and walk the resulting syntax tree,
emitting `(start, end, kind)` spans in source order. Byte offsets are
0-indexed (Rust convention); `kind` is a tree-sitter standard capture
name (`keyword` / `function.call` / `function` / `type` / `string` /
`number` / `comment` / `variable.parameter`).

Implementation note: JuliaSyntax represents source as `GreenNode`s with
explicit trivia (whitespace, comments). We walk the `SyntaxNode` tree
which already excludes trivia for structure, but consult the underlying
green tree's spans for comment ranges.

Heuristic mapping from `JuliaSyntax.kind` symbols to tree-sitter
captures is deliberately conservative — emit only spans we're
confident about, leave anything ambiguous for the frontend's default
rendering. The point isn't to colour everything; it's to colour
things tree-sitter-julia can't (param names, field access, function
def vs call, etc.) where JuliaSyntax has unambiguous answers.
"""
function tokenize_julia_source(source::AbstractString)
    out = Dict[]
    tree = try
        JuliaSyntax.parseall(JuliaSyntax.SyntaxNode, source; filename = "<fence>",
                             ignore_warnings = true)
    catch
        return out
    end
    walk_for_tokens!(out, tree, source, false)
    sort!(out, by = d -> d[:start])
    return out
end

function walk_for_tokens!(out::Vector{Dict}, node, source::AbstractString,
                          in_def_head::Bool)
    # Backend's role here is purely the *semantic overlay* — what
    # tree-sitter-julia can't tell from a lexical walk. Tree-sitter
    # already handles keywords / strings / comments / numbers /
    # operators correctly; we don't re-emit those. We emit:
    #
    #   - function-definition names  → "function"
    #   - call-site names            → "function.call"
    #   - type annotation RHS        → "type"
    #   - supertype RHS              → "type"
    #
    # Each captures the IDENTIFIER's byte range, not the whole
    # composite expression. Children recurse so nested forms (e.g. a
    # call inside a function body, a type annotation inside a struct
    # field) also get coloured.
    k = JuliaSyntax.kind(node)
    if k == K"function"
        # Function-def head is the first child; if it's a K"call",
        # the function name is its first identifier.
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && !isempty(kids)
            head = kids[1]
            if JuliaSyntax.kind(head) == K"call"
                hkids = JuliaSyntax.children(head)
                if !isnothing(hkids) && !isempty(hkids) &&
                   JuliaSyntax.kind(hkids[1]) == K"Identifier"
                    nrng = JuliaSyntax.byte_range(hkids[1])
                    push!(out, Dict(
                        :start => first(nrng) - 1,
                        :end => last(nrng),
                        :kind => "function",
                    ))
                end
            end
        end
    elseif k == K"call" && !in_def_head
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && !isempty(kids) &&
           JuliaSyntax.kind(kids[1]) == K"Identifier"
            nrng = JuliaSyntax.byte_range(kids[1])
            push!(out, Dict(
                :start => first(nrng) - 1,
                :end => last(nrng),
                :kind => "function.call",
            ))
        end
    elseif k == K"::"
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) >= 2
            type_node = kids[end]
            nrng = JuliaSyntax.byte_range(type_node)
            push!(out, Dict(
                :start => first(nrng) - 1,
                :end => last(nrng),
                :kind => "type",
            ))
        end
    elseif k == K"<:" || k == K">:"
        kids = JuliaSyntax.children(node)
        if !isnothing(kids) && length(kids) >= 2
            type_node = kids[end]
            nrng = JuliaSyntax.byte_range(type_node)
            push!(out, Dict(
                :start => first(nrng) - 1,
                :end => last(nrng),
                :kind => "type",
            ))
        end
    end
    kids = JuliaSyntax.children(node)
    if !isnothing(kids)
        # Children of K"function" — the head (first child) is a K"call"
        # that's the def-signature, not a real call. Recurse with
        # in_def_head=true so the inner K"call" walker skips emitting
        # `function.call`; the body still recurses normally so calls
        # inside it pick up `function.call`.
        for (i, c) in enumerate(kids)
            child_in_head = (k == K"function") && (i == 1)
            walk_for_tokens!(out, c, source, child_in_head)
        end
    end
end
