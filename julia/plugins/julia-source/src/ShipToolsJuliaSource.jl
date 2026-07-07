module ShipToolsJuliaSource

# Built-in "core ships as plugins to itself" plugin for `.jl` source files.
# Lives under `julia/plugins/` (per the CLAUDE.md target layout) rather
# than `examples/plugins/` since it's not third-party — the kernel loads
# it eagerly at startup. The reason to ship Julia source preview through
# the same FileType dispatch path as third-party plugins (e.g.
# HDF5Preview) is to keep the ABI honest: if the core code path needs
# something the dispatch surface doesn't expose, the right fix is to
# expand the ABI, not to give core privileged access.

using ConceptExplorerCore
using JSON3
using JuliaSyntax
using JuliaSyntax: @K_str

export JuliaSource

"""
    JuliaSource <: FileType

Built-in plugin for `.jl` Julia source files. Hooks `matches` and
`preview` on the standard `ConceptExplorerCore` dispatch surface; the
kernel's `file.parse` op is still the canonical introspection path for
per-entity AST hashes (this plugin renders the *whole* file as a
structured token stream for the chrome's preview pane).
"""
struct JuliaSource <: ConceptExplorerCore.FileType end

const JULIA_EXTENSIONS = (".jl",)

ConceptExplorerCore.matches(::Type{JuliaSource}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in JULIA_EXTENSIONS)

"""
    preview(::Type{JuliaSource}, path) -> PreviewPayload

Emits `application/vnd.sot.tokens+json` with a `{spans: [{text,
kind}, …]}` payload (per Windows-side bus spec 2026-05-12T15:15Z), so
the frontend renders syntax-coloured Julia source without re-tokenising.

Kind set (matches the frontend's colour map):

- `keyword`  — Julia keywords (`function`/`end`/`if`/`const`/…)
- `comment`  — `#` and `#= … =#`
- `string`   — string/char/cmd literals *including* their delimiters
- `number`   — integer / float / hex / bin / oct literals
- `op`       — operators (`= + == <: |> ::` …)
- `punct`    — `( ) [ ] { } , ;`
- `type`     — uppercase-start identifiers (heuristic; cheap on the
                JuliaSyntax side and good enough for the colour rule)
- `ident`    — every other identifier
- `text`     — whitespace, newlines, and anything not specifically
                classified — required so concatenating every span's text
                reproduces the file byte-for-byte (round-trip
                invariant; frontend can fall back to plain rendering if
                a kind drifts)

Mapping rule: `JuliaSyntax.is_keyword` / `is_operator` / `is_literal`
provide the structural classification; literals are split (`String` /
`Char` / `CmdString` → `string`, others → `number`); punctuation comes
from a small explicit list since JuliaSyntax doesn't expose an
`is_punctuation` predicate. The `String` kind in JuliaSyntax covers the
*content* of a string literal (the inner characters); the quote
delimiters (`"`, `\"\"\"`, `` ` ``) arrive as separate punctuation-shaped
tokens and we explicitly route them to `string` so a string literal
renders as one continuous coloured span.
"""
function ConceptExplorerCore.preview(::Type{JuliaSource}, path::AbstractString)
    bytes = read(path)
    src = String(copy(bytes))
    spans = tokenize_to_spans(src)
    payload = Dict(:spans => spans)
    json = JSON3.write(payload)
    ConceptExplorerCore.PreviewPayload(
        "application/vnd.sot.tokens+json",
        Vector{UInt8}(codeunits(json)),
    )
end

"""
    tokenize_to_spans(src) -> Vector{Dict{Symbol,String}}

Walk `JuliaSyntax.tokenize(src)`, return one span per token. Adjacent
same-kind spans are merged so the wire payload is compact (a function
body's whitespace and identifiers don't blow up the JSON size).

If `JuliaSyntax.tokenize` throws (malformed input is rare but possible),
fall back to a single `text` span covering the whole file so the
frontend still has something to render.
"""
function tokenize_to_spans(src::AbstractString)
    spans = Vector{Dict{Symbol,String}}()
    bytes = codeunits(src)  # JuliaSyntax token ranges are *byte* offsets
    tokens = try
        JuliaSyntax.tokenize(src)
    catch
        return [Dict(:text => String(src), :kind => "text")]
    end
    cursor = 1
    last_kind = ""
    for tok in tokens
        r = tok.range
        # Byte-slice rather than `src[r]` so multi-byte UTF-8 tokens (an
        # `Identifier` token whose range starts mid-codepoint relative to
        # the String index would otherwise blow up with StringIndexError)
        # come through intact.
        text = String(@view bytes[r])
        k = classify_token(tok, text)
        # Coalesce consecutive same-kind spans so the wire stays compact.
        if !isempty(spans) && k == last_kind
            spans[end][:text] = string(spans[end][:text], text)
        else
            push!(spans, Dict(:text => text, :kind => k))
            last_kind = k
        end
        cursor = Int(last(r)) + 1
    end
    # Should be byte-perfect, but if tokenize dropped trailing bytes,
    # emit the remainder so the round-trip invariant still holds.
    if cursor <= length(bytes)
        tail = String(bytes[cursor:end])
        if !isempty(spans) && last_kind == "text"
            spans[end][:text] = string(spans[end][:text], tail)
        else
            push!(spans, Dict(:text => tail, :kind => "text"))
        end
    end
    spans
end

# Map a JuliaSyntax token to one of the nine wire kinds. The text param
# is needed only for the `ident` → `type` uppercase-start heuristic.
function classify_token(tok, text::AbstractString)
    k = JuliaSyntax.kind(tok)
    JuliaSyntax.is_keyword(k) && return "keyword"
    if k == K"Comment"
        return "comment"
    elseif k == K"String" || k == K"Char" || k == K"CmdString"
        return "string"
    elseif JuliaSyntax.is_literal(k)
        return "number"
    elseif k == K"Identifier"
        return _ident_or_type(text)
    elseif _is_string_delimiter(k)
        return "string"
    elseif _is_punct(k)
        return "punct"
    elseif JuliaSyntax.is_operator(k)
        return "op"
    end
    return "text"
end

function _ident_or_type(text::AbstractString)
    isempty(text) && return "ident"
    c = first(text)
    isuppercase(c) ? "type" : "ident"
end

# JuliaSyntax doesn't expose an is_punctuation predicate — explicit list.
# `:` is the operator-flavoured kind; `,` / `;` / brackets are punctuation.
const _PUNCT_KINDS = (K"(", K")", K"[", K"]", K"{", K"}", K",", K";")
_is_punct(k::JuliaSyntax.Kind) = k in _PUNCT_KINDS

# String-literal delimiters: `"`, `"""`, `` ` ``, ``` ``` ```. JuliaSyntax
# kinds these as the literal punctuation strings.
const _STRING_DELIM_KINDS = (K"\"", K"\"\"\"", K"`", K"```", K"'")
_is_string_delimiter(k::JuliaSyntax.Kind) = k in _STRING_DELIM_KINDS

# K"..." comes from JuliaSyntax's exported @K_str, imported explicitly at
# the top so visibility is guaranteed. (Re-declaring the macro locally while
# `using JuliaSyntax` has it in scope is an error on Julia 1.11 — "must be
# explicitly imported to be extended" — and only tolerated by 1.12's binding
# rules; caught by the release pipeline's 1.11 load check.)

"""
    parse_entities(::Type{JuliaSource}, path) -> Vector{ConceptEntity}

Phase-1 stub: returns an empty vector. The kernel's `file.parse` op
already exposes per-entity definitions with `ast_hash`; once the
ConceptEntity hierarchy is wired up in the frontend, this can yield
typed entities (`FunctionEntity`, `StructEntity`, …) keyed off the same
JuliaSyntax walk.
"""
ConceptExplorerCore.parse_entities(::Type{JuliaSource}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
