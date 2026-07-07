module ShipToolsJsonDoc

# Built-in plugin for `.json` files (config, lockfile fragments, generic
# data). Surfaces `application/json` so the frontend can pick a JSON
# renderer (pretty-print, syntax-coloured) rather than the markdown
# buffer; payload is raw UTF-8 bytes.

using ConceptExplorerCore

export JsonDoc

"""
    JsonDoc <: FileType

Built-in plugin for JSON documents. Same plugin shape as siblings under
`julia/plugins/`. No `parse_entities` for phase 1 — JSON's concept
surface (object keys, paths) isn't wired into the ConceptEntity
hierarchy yet.
"""
struct JsonDoc <: ConceptExplorerCore.FileType end

const JSON_EXTENSIONS = (".json",)

ConceptExplorerCore.matches(::Type{JsonDoc}, path::AbstractString) =
    any(endswith(lowercase(path), ext) for ext in JSON_EXTENSIONS)

function ConceptExplorerCore.preview(::Type{JsonDoc}, path::AbstractString)
    bytes = read(path)
    ConceptExplorerCore.PreviewPayload("application/json", bytes)
end

ConceptExplorerCore.parse_entities(::Type{JsonDoc}, path::AbstractString) =
    ConceptExplorerCore.ConceptEntity[]

end # module
