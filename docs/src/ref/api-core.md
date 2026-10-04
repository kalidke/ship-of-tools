# API — ConceptExplorerCore

```@meta
CurrentModule = ConceptExplorerCore
```

`ConceptExplorerCore` is the plugin ABI: the abstract types every extension
dispatches on, the two structs that cross the Rust↔Julia boundary, and the
contract functions. For the narrative version with examples, see
[The Dispatch ABI](../extend/abi.md).

Of these, `FileType`, `preview` and `matches` are built; the rest are declared
design targets with no implementation.

```@index
Modules = [ConceptExplorerCore]
```

```@autodocs
Modules = [ConceptExplorerCore]
```
