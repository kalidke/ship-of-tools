# Browser-served artifacts: BrowserView, the WGLMakie/Bonito server, wglshow.

"""
    BrowserView(url)

Marker wrapping a loopback URL for a live, browser-served artifact (an
interactive WGLMakie/Bonito figure, a served dashboard, …). Return one as the
last expression of an eval — or call [`browserview`](@ref) — and the REPL emits
a `browser` frame instead of a static `value`/`image`, which the frontend hands
to the OS browser-open (ADR 0032). `url` must be loopback-shaped
(`http://127.0.0.1:<port>/…`) so it resolves through the launcher's `-L` tunnel
on a remote frontend; the WGLMakie/Bonito port is `SOT_WGL_PORT` (default 1241,
forwarded by the launcher alongside pluto/video/docs — 1237–1240 are the docs
pool, so WGL sits at 1241).

`BrowserView` deliberately carries no plotting dependency: the Bonito server
that produces `url` lives in the *user's* project env (whichever WGLMakie they
`using`), so `ShipToolsRepl` stays lightweight and backend-agnostic.
"""
struct BrowserView
    url::String
    """
    Whether front-ends should auto-open the URL. `false` = serve-only: the
    browser frame still flows (so the daemon's ADR-0035 proxy allowlist
    learns the port) but NO frontend opens a tab — the caller then targets
    exactly one FE with `sot-fe open-url <url> --fe <handle>`. The escape
    from the multi-FE broadcast-open, where two live clients race one
    shared Figure layout (`resize_to = :parent`).
    """
    open::Bool
end
BrowserView(url::AbstractString) = BrowserView(String(url), true)

"""
    browserview(url; open = true) -> BrowserView

Convenience constructor. `return browserview(server_url)` at the end of an eval
to open `url` in the frontend's browser; `open = false` serves without opening
anywhere (see [`BrowserView`](@ref)).
"""
browserview(url::AbstractString; open::Bool = true) = BrowserView(String(url), open)

# Tracks the Bonito server started by `wglshow` so a repeat call frees the port
# instead of hitting EADDRINUSE. `Any` — ShipToolsRepl never loads Bonito.
const WGL_SERVER = Ref{Any}(nothing)

# WGLMakie's General-registry UUID, used to look it up in Base.loaded_modules
# (not Main — see wglshow) regardless of how it entered the REPL's world.
const WGLMAKIE_PKGID = Base.PkgId(
    Base.UUID("276b4fcb-3e11-5398-bf8b-a0c2d153d008"), "WGLMakie")

# Pushed onto the display stack at REPL boot (see `serve`) so `display(fig)`
# routes to `wglshow` without ShipToolsRepl ever loading WGLMakie itself — see
# "Display-stack integration" in the `wglshow` docstring. No `display` methods
# are defined on this type here; the ShipToolsReplWGLMakieExt package
# extension (loaded only when WGLMakie is) adds the one Makie-figure method,
# so Base.display(x) falls through this display for every other value exactly
# like it would with no display pushed at all.
struct WGLDisplay <: Base.AbstractDisplay end

# Base-only (no Makie): claiming text/html displayability makes
# `Multimedia.has_html_display()` true the moment this is pushed. Bonito's own
# `__init__` checks exactly that and pushes its own `BrowserDisplay` ONLY when
# it's false — without this, Bonito's display would land ABOVE WGLDisplay on
# the stack (pushed later, at WGLMakie/Bonito load time) and capture every
# `display(fig)` itself (xdg-open, useless on a headless backend) before
# WGLDisplay ever saw it. No other MIME is claimed — this display renders
# nothing on its own; it exists only to keep Bonito off the top of the stack.
Base.displayable(::WGLDisplay, ::MIME"text/html") = true

# Client-side error overlay injected into every wglshow page. Bonito already
# turns Julia-side render errors into inline error HTML, but a WebGL/JS error
# (e.g. WGLMakie/THREE "computeBoundingBox NaN" from an under-constrained scene)
# only hits the browser console and leaves a blank canvas. This runs at parse
# time and surfaces window errors, unhandled rejections, and console.error into
# a dismissible panel, so a broken render shows WHAT went wrong in the page.
# No `\$` (Julia interpolation) and no JS template literals by design.
const WGL_ERROR_OVERLAY_JS = """
(function () {
  if (window.__wglshowErrHooked) return;
  window.__wglshowErrHooked = true;
  function box() {
    var id = "wglshow-error-overlay";
    var el = document.getElementById(id);
    if (el) return el;
    el = document.createElement("div");
    el.id = id;
    el.style.cssText = "position:fixed;left:0;right:0;bottom:0;max-height:55%;overflow:auto;z-index:2147483647;margin:0;padding:14px 40px 14px 16px;white-space:pre-wrap;word-break:break-word;font:13px/1.45 ui-monospace,Menlo,Consolas,monospace;color:#ffdada;background:rgba(43,20,22,0.97);border-top:2px solid #ff6b6b;box-shadow:0 -2px 12px rgba(0,0,0,0.5)";
    var x = document.createElement("div");
    x.textContent = "×";
    x.title = "dismiss";
    x.style.cssText = "position:absolute;top:6px;right:12px;cursor:pointer;color:#ff9b9b;font-weight:bold;font-size:18px";
    x.onclick = function () { el.remove(); };
    el.appendChild(x);
    var h = document.createElement("div");
    h.textContent = "wglshow — render error (browser side):";
    h.style.cssText = "font-weight:bold;margin-bottom:6px;color:#ff9b9b";
    el.appendChild(h);
    var b = document.createElement("div");
    b.id = id + "-body";
    el.appendChild(b);
    (document.body || document.documentElement).appendChild(el);
    return el;
  }
  function push(msg) {
    try {
      box();
      var b = document.getElementById("wglshow-error-overlay-body");
      var line = document.createElement("div");
      line.textContent = String(msg);
      line.style.cssText = "margin:2px 0;padding-top:4px;border-top:1px solid rgba(255,255,255,0.08)";
      b.appendChild(line);
    } catch (e) {}
  }
  window.addEventListener("error", function (e) {
    push((e && e.message ? e.message : String(e)) +
         (e && e.filename ? "  [" + e.filename + ":" + e.lineno + "]" : ""));
  });
  window.addEventListener("unhandledrejection", function (e) {
    var r = e && e.reason;
    push("unhandled promise rejection: " + (r && r.message ? r.message : String(r)));
  });
  var orig = console.error;
  console.error = function () {
    try { push(Array.prototype.map.call(arguments, String).join(" ")); } catch (e) {}
    return orig.apply(console, arguments);
  };
})();
"""

# Interactive Makie widget Blocks that need a JS→Julia event round-trip wglshow
# can't provide (they render but never respond over Bonito). Matched by type
# NAME — the shim carries no Makie dependency, so it never references the types.
const WGL_WIDGET_TYPES = (:Button, :Menu, :Slider, :IntervalSlider, :Toggle, :Textbox)

# The figure's block/plot content vector, or `nothing` if `fig` isn't a
# Figure-like object we can introspect. invokelatest: Makie's `getproperty` was
# defined by the user's `using` after wglshow's world age.
function wgl_figure_content(fig)
    getters = (f -> Base.invokelatest(getproperty, f, :content),
               f -> Base.invokelatest(getproperty,
                        Base.invokelatest(getproperty, f, :figure), :content))
    for get in getters
        try
            c = get(fig)
            c isa AbstractVector && return c
        catch
        end
    end
    return nothing
end

# Per maintainer directive: wglshow NEVER uses Makie's interactive HTML widgets,
# but it must not disable them *silently*. If the figure actually carries
# interactive widgets, warn the author (on the captured stderr, so it surfaces
# in the REPL) that they will render but not respond over the browser.
function wgl_warn_if_widgets(fig)
    content = wgl_figure_content(fig)
    content === nothing && return
    found = String[]
    for x in content
        n = nameof(typeof(x))
        n in WGL_WIDGET_TYPES && push!(found, String(n))
    end
    isempty(found) && return
    kinds = join(sort(unique(found)), ", ")
    println(stderr,
        "wglshow: WARNING — this figure has interactive Makie widgets ($kinds) that " *
        "will render but NOT respond over the browser. wglshow does not use Makie's " *
        "HTML widgets (they don't round-trip reliably over Bonito and crash " *
        "under-constrained scenes), and this is not silently overridable. Drive " *
        "controls from the figure itself (e.g. pixel hit-testing on scene mouse events).")
    flush(stderr)
end

# Preferred-then-ephemeral port pick for the wglshow Bonito server (mirrors
# the daemon content servers' 2026-07-23 shared-host collision fix): try the
# preferred port; when it's taken (another user's server on a shared host, or
# ANOTHER WORKSPACE's REPL child of this same user — every child prefers the
# same SOT_WGL_PORT), fall back to an OS-assigned ephemeral port. The daemon
# learns the actual port from the BrowserView frame this serve emits (it
# allowlists it for the ADR-0035 proxy), and the FE arms its proxy listener
# per-URL — so an ephemeral port reaches the browser with no other change.
# The probe-close-rebind window is a benign TOCTOU: losing it just fails
# Bonito's own bind loudly.
function wgl_pick_port(preferred::Int)
    try
        srv = Sockets.listen(Sockets.InetAddr(ip"127.0.0.1", preferred))
        close(srv)
        return preferred
    catch
        srv = Sockets.listen(Sockets.InetAddr(ip"127.0.0.1", 0))
        _, p = Sockets.getsockname(srv)
        close(srv)
        @warn "wglshow: preferred WGL port taken — using an ephemeral port (multi-user host, or a second workspace serving?)" preferred port = Int(p)
        return Int(p)
    end
end

"""
    wglshow(fig; port = nothing, open = true) -> BrowserView

Serve an interactive WGLMakie figure over Bonito on a loopback port and return a
[`BrowserView`](@ref), so the frontend auto-opens it in the browser (ADR 0032).

`open = false` serves WITHOUT opening a browser anywhere: the frame still flows
(the daemon's proxy allowlist learns the port) and the collected output prints
the URL plus the targeted follow-up (`sot-fe open-url <url> --fe <handle>`), so
a session can put the figure on exactly ONE frontend. Use it when multiple
frontends are attached — two live browser clients on one served figure race the
shared layout (`resize_to = :parent`), corrupting axis placement and hitboxes.
Call it as the last expression of an eval:

    using WGLMakie
    fig = surface(-10:0.4:10, -10:0.4:10, (x, y) -> sin(x) + cos(y);
                  axis = (; type = Axis3))
    wglshow(fig)

`ShipToolsRepl` carries no plotting dependency: WGLMakie/Bonito are resolved at
call time by PkgId from `Base.loaded_modules` — WGLMakie just needs to be
*loaded* in this REPL's world (directly via `using WGLMakie`, or transitively
through a package that depends on it; Bonito then comes in as WGLMakie's own
dependency). The server binds `127.0.0.1` on the preferred port
(`SOT_WGL_PORT`, default 1241) and falls back to an OS-assigned ephemeral port
when it's taken — the browser reaches either through the frontend's per-URL
ADR-0035 proxy (or a launcher `-L` forward for the preferred port). It lives as
long as the REPL; a repeat `wglshow` replaces it. Pass `port` explicitly to pin
a port verbatim (no fallback — a taken pinned port errors loudly).

The figure fills the browser window and grows with it as the window is resized
(`resize_to=:parent` mounted in a viewport-filling container).

## Display-stack integration

A consumer never has to name `wglshow` (or a backend) at all: once WGLMakie is
loaded — directly or transitively — plain `display(fig)` on a Makie figure
routes here automatically, via a `WGLDisplay` pushed onto the display stack at
REPL boot and a package extension (`ShipToolsReplWGLMakieExt`, loaded only
when WGLMakie is) that teaches it to render a figure. `wglshow(fig; port=…)`
keeps working exactly as documented above for anyone who wants the explicit
call (e.g. to pin a port).

Pinned against WGLMakie 0.13 / Bonito 5.1 (validated live, ADR 0032).
"""
function wglshow(fig; port::Union{Integer,Nothing} = nothing, open::Bool = true)
    WGL = get(Base.loaded_modules, WGLMAKIE_PKGID, nothing)
    WGL === nothing && error("wglshow: WGLMakie is not loaded in this REPL — load it directly (`using WGLMakie`) or through a package that depends on it")
    # Bonito arrives as WGLMakie's dependency; require it by UUID (already loaded,
    # so this just returns the module) rather than assume the user `using`d it.
    Bonito = Base.require(Base.PkgId(
        Base.UUID("824d6782-a2ef-11e9-3a09-e5662e0c26f8"), "Bonito"))
    host = "127.0.0.1"
    # Close the previous server BEFORE picking the port, so a repeat wglshow in
    # this child finds its own old port free and reuses it (stable URL across
    # re-serves) instead of needlessly falling back to a fresh ephemeral port.
    prev = WGL_SERVER[]
    if prev !== nothing
        WGL_SERVER[] = nothing
        try
            Base.invokelatest(close, prev)
        catch
        end
    end
    # Explicit `port` is honored verbatim (a taken port errors loudly — the
    # caller asked for exactly that one); the default probes SOT_WGL_PORT
    # (1241) and falls back to an ephemeral port when it's taken.
    port = port === nothing ?
        wgl_pick_port(parse(Int, get(ENV, "SOT_WGL_PORT", "1241"))) : Int(port)
    external = "http://$host:$port"
    # Warn (never silently) if the figure carries interactive Makie widgets —
    # wglshow can't make them respond over the browser (see wgl_warn_if_widgets).
    wgl_warn_if_widgets(fig)
    # invokelatest throughout: these methods were defined by the user's `using`
    # after wglshow's world age (same reason value_frames_for uses it).
    #
    # wglshow NEVER uses Makie's interactive HTML widgets: force
    # use_html_widgets=false here so no user theme/config can turn them on
    # through wglshow. They do not round-trip reliably over Bonito and their
    # layout pass crashes under-constrained scenes (pixel-only Axes / dummy-mesh
    # LScenes) with a NaN bounding box. Interactive controls belong to the
    # figure author (hand-rolled pixel hit-testing), not this serving layer.
    # activate!'s set_screen_config! resets per call, so setting it here wins.
    Base.invokelatest(WGL.activate!; resize_to = :parent, use_html_widgets = false)
    Base.invokelatest(Bonito.configure_server!;
        listen_url = host, listen_port = port, proxy_url = external)
    # Mount the figure in a viewport-filling container so a resize_to=:parent
    # figure grows with the browser window instead of Bonito's content-sized
    # default (which pinned it to ~1/3 width — ImagingSystemDesign finding, 2026-07-13).
    #
    # Bonito already renders Julia-side render errors (a throw in the App handler
    # or in the figure's jsrender) as inline error HTML — see its
    # `handle_render_error`. What it CANNOT catch is a client-side WebGL/JS error
    # (e.g. WGLMakie/THREE "computeBoundingBox NaN" from an under-constrained
    # scene), which only hits the browser console and leaves a blank canvas. So
    # we inject a tiny client-side error overlay that surfaces those into the
    # page too (maintainer request: show render errors in the html render).
    app = Base.invokelatest(Bonito.App, () ->
        Bonito.DOM.div(
            Bonito.DOM.script(WGL_ERROR_OVERLAY_JS),
            Bonito.DOM.div(fig; style = "position:fixed; inset:0; margin:0")))
    server = Base.invokelatest(Bonito.Server, app, host, port; proxy_url = external)
    WGL_SERVER[] = server
    url = Base.invokelatest(Bonito.online_url, server, "/")
    # Announce AT SERVE TIME (browser frame now), not only via the last-value
    # path — a wrapper that swallows this return value would otherwise make
    # the serve invisible (port never allowlisted, FE never opens the page).
    # With `open = false` the frame still flows (allowlist intact) but no
    # frontend opens a tab; print the targeted-open recipe so the caller has
    # the URL and the follow-up command in the collected output.
    if !open
        println("wglshow: serving (no auto-open) at $url — open on ONE frontend with:")
        println("  sot-fe open-url '$url' --fe <fe-handle>")
    end
    return announce_browserview(BrowserView(url, open))
end
