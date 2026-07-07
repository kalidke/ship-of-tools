module ShipToolsMarkdown

# Built-in plugin for Markdown documents (`.md`, `.markdown`). Same
# "core ships as plugins to itself" mandate as ShipToolsJuliaSource — the
# frontend's comrak renderer consumes the `text/markdown` mime
# directly, so this plugin's job is just to claim the path and hand
# back UTF-8 bytes with the right mime.

using ConceptExplorerCore

export MarkdownDoc

"""
    MarkdownDoc <: FileType

Built-in plugin for Markdown files. Renders as `text/markdown`; the
frontend's existing markdown renderer (`preview/markdown.rs` with
comrak) does the actual layout and inline-math rendering.
"""
struct MarkdownDoc <: ConceptExplorerCore.FileType end

const MARKDOWN_EXTENSIONS = (".md", ".markdown")

ConceptExplorerCore.matches(::Type{MarkdownDoc}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in MARKDOWN_EXTENSIONS)

"""
    preview(::Type{MarkdownDoc}, path) -> PreviewPayload

Returns the file as `text/markdown`. The frontend renderer (comrak +
cosmic-text) consumes that mime today; routing through this plugin
means future per-mime hooks (e.g. inline-math placement, link
verification) get a single place to extend without touching backend or
frontend.
"""
function ConceptExplorerCore.preview(::Type{MarkdownDoc}, path::AbstractString)
    bytes = read(path)
    ConceptExplorerCore.PreviewPayload("text/markdown", bytes)
end

"""
    parse_entities(::Type{MarkdownDoc}, path) -> Vector{ConceptEntity}

Phase-1 stub. A future revision could parse the heading hierarchy
(`#`/`##`/…) and yield section entities, anchoring `.concept/`
annotations to specific markdown sections.
"""
ConceptExplorerCore.parse_entities(::Type{MarkdownDoc}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
