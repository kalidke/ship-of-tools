# Math + figure sample

Demonstrates the three preview-pane content kinds in one document:
prose, math (inline and display), and an embedded image. Today the
image and inline math arms are stubs in the comrak walk — this file
becomes the target once both ship inline.

## A simple function and its plot

Consider the surface

$$
z(x, y) = \sin(x) \cos(y)
$$

with $x, y \in [-\pi, \pi]$. The contour map below was rendered with
CairoMakie at the acceptance-gate spike.

![Sample heatmap from the M1 spike](sample.png)

## Why this matters

The "concept explorer" idea hinges on jumping fluidly between **levels
of abstraction** — module, type, function, output, math — without
losing the reader's place. A markdown document that mixes prose with
$O(n \log n)$ complexity notes, a generated figure, and a derivation
like

$$
\frac{d}{dx}\left[ \sin(x) \cos(y) \right] = \cos(x)\cos(y)
$$

is the canonical example.

## What still needs to ship

1. **Inline `$…$` flow** inside the markdown buffer — currently a
   literal `$x$` string lands in the cosmic-text spans.
2. **Image embedding** — the comrak walk's `Image` arm is a no-op
   today (children are walked, alt text emits via the `Text` arm but
   no actual `<img>` lands). Needs a sidecar load + a wgpu quad keyed
   off the surrounding rect.
3. **Vertical positioning of figures** so a `![…](…)` reads as "right
   here" rather than appended at the bottom of the pane.

Until then this file is a forward-looking spec rather than something
that renders beautifully.
