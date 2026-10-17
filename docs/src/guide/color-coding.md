# Color Coding

!!! note "Status: partly built"
    What renders today is the per-session **agent work-state** color (idle · working · blocked · waiting · done, on the Sessions strip and nav rows) and the **stale-annotation** drift badge, which is per file (the file's content hash against the annotation's `synced_against`) and shows on the selected file row in Files mode. The per-entity provenance states in the table below (user-edited, agent-edited accept/reject, immutable/external, favorited) are planned; a pinned file already renders, as a bright cyan row in the navigation tree rather than an accented border.

Color coding is a cross-cutting layer. The same entity carries the same
provenance no matter which mode you view it in, so Ship of Tools renders that provenance
**uniformly across all modes** — in the design, a stale annotation looks stale in
every mode, because staleness belongs to the entity, not to the view. Built today,
staleness is a property of a *file* and shows on that file's row in Files mode.

Color and badging are orthogonal to mode. A mode decides *what tree to show*;
the color layer decides *how each node is tinted* based on who touched it, when,
and whether its annotation still matches the code.

```@raw html
<DemoShot name="state-colors" caption="Session rows in their work-state colors, each with the reason it last reported." />
```

## The states

| State | Appearance | Meaning | Built today |
|-------|------------|---------|-------------|
| User-edited recently | Warm color, fades over time | You touched this entity lately; the warmth decays as it ages. | No |
| Agent-edited, unaccepted | Distinct color (unresolved diff) | An agent changed this and you have not yet accepted or discarded the change. | No |
| Agent-edited, accepted | Neutral, small sigil | An agent's change that you accepted; marked with a small sigil but otherwise neutral. | No |
| Immutable / external | Dimmed | Base, dependencies, vendored code — not yours to edit, rendered dimmed. | No |
| Stale annotation | Yellowed | The entity's [concept annotation](concept-layer.md) was written against an older version of its file (the file's content hash differs from `synced_against`). | Yes, per file, on the selected row in Files mode; per-entity staleness is not built |
| Pinned file | Bright cyan, bold row | You pinned this file to keep its preview while browsing. | Yes, in the navigation tree; no other entity can be pinned or favorited |
| Agent work state | Working green, blocked red, waiting purple, done blue, idle gray | A session's work state; see [Work-state colors](../concepts/work-state.md). | Yes, on Sessions rows and the session strip |

## Why it is mode-independent

Provenance is a fact about an entity: who edited it, when, whether its annotation
is current. None of those facts change when you switch the root tree from Files
to Modules. Encoding them once, at the entity level, means you do not relearn a
color scheme per mode — in Files mode a yellowed badge means "the prose may not match
the code." (A Sessions row is also drawn yellow when its capsule is held by a different build.)

The "stale annotation" state is the direct rendering of the staleness detection
described in [The Concept Layer](concept-layer.md): when an annotation's
`synced_against` hash no longer matches the current content hash of its file, the
file's row yellows until you refresh it. In the design the same yellow follows the
entity into every mode; today it is drawn for the selected file row in Files mode
only.
