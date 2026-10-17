# API — ConceptExplorerCore

```@meta
CurrentModule = ConceptExplorerCore
```

`ConceptExplorerCore` is the plugin ABI: the abstract types every extension
dispatches on, the two structs `TreeNode` and `PreviewPayload`, and the
contract functions. For the narrative version with examples, see
[The Dispatch ABI](../extend/abi.md).

Built and used: `FileType`, `matches`, `preview`, `file_types`, `file_type_for`,
`PreviewPayload` (every plugin's `preview` returns one) and `read_within` (the PDF
and video plugins run their tools through it). `parse_entities` has a method in
every built-in plugin and no caller. Declared and not built: `Mode`, `ConceptEntity`,
`AnnotationKind`, `Tool`, `Capture`, `TreeNode` (nothing outside core's tests builds
one), `ast_hash` and `applicable_annotations` (no method). Which structs cross
the Rust↔Julia seam is in [the serialization seam](@ref abi-serialization-seam).

```@index
Modules = [ConceptExplorerCore]
```

```@autodocs
Modules = [ConceptExplorerCore]
```
