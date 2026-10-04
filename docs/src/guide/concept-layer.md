# The Concept Layer

The concept layer is what makes Ship of Tools a *concept explorer* rather than a file
browser. It is prose — intent, contracts, meaning, math — attached to the units
of a Julia project and kept honest against the code those units describe. The
LLM is the primary author and maintainer of this layer; the user can edit it
too.

```@raw html
<DemoShot name="concept-stale" caption="The annotation line marks the file's annotation STALE: the file changed after the annotation was written." />
```

It is built from two layers with very different update mechanics. Keep them
separate in your head:

| Layer | Source | Currency | LLM involved? |
|-------|--------|----------|---------------|
| **Structural** | parsed mechanically from code via `JuliaSyntax.jl` (plus live introspection where the project is loaded) | always current | no |
| **Annotation** | LLM- or user-authored prose attached to nodes in the structural layer | can drift | yes |

The structural layer is derived, never stored as a fact you can get wrong — it
is recomputed from the source. The annotation layer is the part that can fall
out of sync, so the whole design below is about *detecting and surfacing that
drift* rather than trying to prevent it.

## The `.concept/` sidecar

Annotations live in a sidecar `.concept/` directory at the project root, mirroring
the conceptual structure of the project:

```text
.concept/
  project/intent.md
  modules/MyModule.md
  types/MyModule/MyType.md
  functions/MyModule/myfunction.md
  math/geometry/rotation.md
```

The directory is plain files on disk. That is deliberate: it survives a restart,
diffs cleanly in git, and is readable without Ship of Tools running. Each path
corresponds to a node you can navigate to in a mode — a module in Modules mode,
a type in Types mode, a derivation in Math mode — so the annotation for a node
is always at a predictable location.

## Frontmatter

Each annotation file carries YAML frontmatter that ties the prose to the entity
it describes and records when it was last reconciled:

```yaml
target: MyModule.MyType
target_kind: type
synced_against: <file hash>
synced_at: 2026-01-15T14:30Z
authored_by: an agent | the user
references:
  - MyModule.method1
  - math/geometry/rotation
```

| Field | Meaning |
|-------|---------|
| `target` | the entity this annotation describes |
| `target_kind` | what kind of entity (`type`, `function`, `module`, …) |
| `synced_against` | the content hash of the target's file at the last reconciliation |
| `synced_at` | timestamp of that reconciliation |
| `authored_by` | free text, an agent or the user; nothing reads it today (the provenance colouring it is meant to feed is not built — see [Color Coding](color-coding.md)) |
| `references` | links to other entities or annotations (verification is planned — see below) |

The load-bearing field is `synced_against`. It is the hash of the target's file as
of the last time the annotation was confirmed accurate. Staleness is just a
comparison: take the file's hash now, and if it differs from `synced_against`,
the prose was written against an older version of the code.
Today `synced_against` is the only field the backend parses; the rest of the
frontmatter is preserved verbatim across edits and shown above the editor, but is
not otherwise acted on.

## The staleness hash

What is built is a **file-level** hash. The kernel's `file.parse` reply carries
an `ast_hash` field that is the SHA-256 of the file's bytes, rendered as the full
64-character hex digest. Despite the field name, nothing is parsed to make it, so
any change to the file — a reformat, a comment, a docstring — changes the hash,
and one hash covers the whole file, however many entities it declares.

The window keeps that hash per file and compares it with the `synced_against` of
the annotation for the file (target `files/<path>`). The daemon parses only
`synced_against` out of the frontmatter, and gates a concept write on it: a save
carries the `synced_against` the editor opened with, and the daemon refuses the
write if the annotation on disk has moved on.

!!! warning "NOT BUILT: the per-entity hash"
    ADR 0005 (`docs/adr/0005-ast-hash.md`) specifies a hash of the *targeted entity's*
    parsed syntax tree, with trivia skipped, so that reformatting does not mark an
    annotation stale while a structural edit does, and so that an edit elsewhere in
    the file leaves it alone. That contract is not built. The kernel does compute a
    per-definition `ast_hash` of that shape in its definition lists (`file.parse`
    definitions, project scans), but nothing compares those values with
    `synced_against` and no annotation is keyed to a single definition.

## Update lifecycle

Drift detection is reactive — Ship of Tools surfaces it, you fix it when you choose to:

1. **You save a file.** (Either you edited it, or an agent did.)
2. **The window asks the kernel for the file's hash** (`file.parse`) when the
   cursor reaches the file's row in Files mode, once per file, and keeps it. No
   save event re-hashes a file today.
3. **The annotation is marked stale when the hashes differ.** The comparison is
   `file_hash != synced_against`, made for the annotation of the file under the
   cursor.
4. **The stale annotation renders yellowed** on that file's row and in the
   annotation status line. See [Color Coding](color-coding.md).

Not built: per-entity staleness (a changed entity marking only its own annotation
stale) and the badge in every mode. Both rest on the per-entity hash above.

There is no background sweep in phase 1. Nothing recomputes annotations on a
timer or refreshes them behind your back. Visible drift is the feature: a
yellowed badge tells you the prose may no longer match the code.

## Reactive refresh

The intended model is reactive: navigate to the stale annotation and trigger a
refresh with a single keypress that re-stamps `synced_against` (and `synced_at`)
to the file's current hash, marking the prose as reconciled against the present
code. You stay in control of *when* — a refresh asserts the annotation is still
accurate, so it is a deliberate act, not an automatic one.

That one-key re-stamp is not yet built. Today you reconcile a stale annotation by
editing it in place (`e`), updating its `synced_against` in the frontmatter, and
saving (`Ctrl+S`); `Esc` discards. A background staleness sweep is also
explicitly deferred; refresh is on-demand only.

## Reference verification (planned)

The `references` list links an annotation to other entities (`MyModule.method1`)
or to other annotations (`math/geometry/rotation`). A background pass that
verifies these links after every re-index is planned: when a referenced entity no
longer exists — a method was deleted, a derivation renamed — the link would be
broken, and the annotation marked stale on that basis too, keeping the concept
layer internally consistent, not just consistent with code. Today the
`references` field is preserved on disk but not verified.

## See also

- [Color Coding](color-coding.md) — how staleness and authorship render across modes.
