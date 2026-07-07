module ShipToolsPlainText

# Built-in plugin for `.txt` files. Surfaces `text/plain` so the frontend
# can route to a code/text renderer rather than the markdown buffer. The
# plugin claims a narrow extension set on purpose: `JuliaSource` /
# `MarkdownDoc` / `TomlDoc` / `JsonDoc` already cover their formats, and
# turning this into a "claims anything UTF-8" catch-all would defeat
# their matching — the dispatch picks the first matching plugin, so a
# broad claim here would shadow the more-specific built-ins.

using ConceptExplorerCore

export PlainText

"""
    PlainText <: FileType

Built-in plugin for `.txt` documents. Hooks `matches` + `preview` on the
standard dispatch surface; payload is raw UTF-8 bytes with the
`text/plain` mime.
"""
struct PlainText <: ConceptExplorerCore.FileType end

const PLAIN_TEXT_EXTENSIONS = (".txt",)

ConceptExplorerCore.matches(::Type{PlainText}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in PLAIN_TEXT_EXTENSIONS)

function ConceptExplorerCore.preview(::Type{PlainText}, path::AbstractString)
    bytes = read(path)
    ConceptExplorerCore.PreviewPayload("text/plain", bytes)
end

ConceptExplorerCore.parse_entities(::Type{PlainText}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
