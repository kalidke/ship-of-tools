module ShipToolsTomlDoc

# Built-in plugin for `.toml` files (Project.toml, Manifest.toml, generic
# config). Surfaces `text/x-toml` so the frontend can pick a code/text
# renderer rather than the markdown buffer; payload is raw UTF-8 bytes.

using ConceptExplorerCore

export TomlDoc

"""
    TomlDoc <: FileType

Built-in plugin for TOML documents. Same shape as `MarkdownDoc` /
`JuliaSource`: declarative `matches` + bytes-passthrough `preview`. No
parse step yet — `parse_entities` returns empty; the kernel's
`file.parse` path is Julia-source-specific and TOML doesn't have an
analogous concept-entity surface in phase 1.
"""
struct TomlDoc <: ConceptExplorerCore.FileType end

const TOML_EXTENSIONS = (".toml",)

ConceptExplorerCore.matches(::Type{TomlDoc}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in TOML_EXTENSIONS)

function ConceptExplorerCore.preview(::Type{TomlDoc}, path::AbstractString)
    bytes = read(path)
    ConceptExplorerCore.PreviewPayload("text/x-toml", bytes)
end

ConceptExplorerCore.parse_entities(::Type{TomlDoc}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
