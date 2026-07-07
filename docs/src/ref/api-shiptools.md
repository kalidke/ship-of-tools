# API — ShipTools

```@meta
CurrentModule = ShipTools
```

The `ShipTools` umbrella package. In phase 1 its public Julia surface is the
installer for the [sot-comm](comm.md) session-to-session messaging system;
the explorer itself runs as the Rust frontend and backend plus the Julia kernel
and plugins, which are documented under [Design & Internals](../design/requirements.md)
and [Extending Ship of Tools](../extend/abi.md).

```@index
Modules = [ShipTools]
```

```@autodocs
Modules = [ShipTools]
```
