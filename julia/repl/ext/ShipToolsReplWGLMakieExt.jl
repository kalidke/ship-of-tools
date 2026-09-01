module ShipToolsReplWGLMakieExt

# Package extension: loads ONLY once the consumer's own `using`/dependency
# graph has already loaded WGLMakie (directly or transitively) — see
# "Display-stack integration" in ShipToolsRepl.wglshow's docstring and the
# dated amendment in docs/adr/0032-interactive-browser-figures.md. This is
# the ONLY place that teaches `ShipToolsRepl.WGLDisplay` how to render a
# figure; the base package pushes the display but defines no method on it,
# so without this extension `display(fig)` falls through to whatever display
# is next on the stack, exactly as if nothing had been pushed.

using ShipToolsRepl
using WGLMakie

const Makie = WGLMakie.Makie

Base.display(::ShipToolsRepl.WGLDisplay, fig::Makie.FigureLike) = (ShipToolsRepl.wglshow(fig); nothing)

function __init__()
    # Makie's own one-argument `display(fig)` bypasses the display stack
    # unless the active backend is inline — the backend here is headless, so
    # the display stack (→ the browser, via WGLDisplay above) IS the display.
    # Scoped to this extension's __init__, never the ShipToolsRepl shim boot,
    # so it only takes effect once WGLMakie is actually loaded.
    Makie.inline!(true)
end

end
