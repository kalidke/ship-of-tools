# Math sample

A short tour of inline and display math the preview pane should one day
typeset. Today most of these will arrive as literal `$…$` text — inline
math is phase-2, while display math currently lives in
the math pane via `math.render`. This file is here for when the chrome
stitches them together inline.

## Inline

The quadratic formula is $x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}$, and
Euler's identity, $e^{i\pi} + 1 = 0$, packs five fundamental constants
into a single equation.

A sum like $\sum_{k=1}^{n} k = \tfrac{n(n+1)}{2}$ shows up everywhere
once you start counting.

## Display

The Gaussian integral:

$$
\int_{-\infty}^{\infty} e^{-x^2}\, dx = \sqrt{\pi}
$$

Maxwell's equations in differential form:

$$
\begin{aligned}
\nabla \cdot \mathbf{E} &= \tfrac{\rho}{\varepsilon_0} \\
\nabla \cdot \mathbf{B} &= 0 \\
\nabla \times \mathbf{E} &= -\tfrac{\partial \mathbf{B}}{\partial t} \\
\nabla \times \mathbf{B} &= \mu_0\mathbf{J} + \mu_0\varepsilon_0\tfrac{\partial \mathbf{E}}{\partial t}
\end{aligned}
$$

## Why the file exists

- **Round-trip test** — once inline `$…$` flow ships, this file
  shouldn't change. Same source, better render.
- **Heading hierarchy** — h1/h2 should typeset at distinct sizes
  via the comrak walk's metric overrides.
- **Mixed content** — prose, lists, and math in one file so the
  renderer's branching is exercised.
