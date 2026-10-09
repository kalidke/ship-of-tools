# Browser-served artifacts: BrowserView, the WGLMakie/Bonito server, wglshow.

"""
    BrowserView(url)

Marker wrapping a loopback URL for a live, browser-served artifact (an
interactive WGLMakie/Bonito figure, a served dashboard, …). Return one as the
last expression of an eval — or call [`browserview`](@ref) — and the REPL emits
a `browser` frame instead of a static `value`/`image`, which the frontend hands
to the OS browser-open (ADR 0032). `url` must be loopback-shaped
(`http://127.0.0.1:<port>/…`) so a remote frontend reaches it through the
daemon's page proxy (ADR 0035). A `BrowserView` of a server your own code
started carries no protection of its own; [`wglshow`](@ref) serves with a
secret in the address.

`BrowserView` deliberately carries no plotting dependency: the Bonito server
that produces `url` lives in the *user's* project env (whichever WGLMakie they
`using`), so `ShipToolsRepl` stays lightweight and backend-agnostic.
"""
struct BrowserView
    url::String
    """
    Whether front-ends should auto-open the URL. `false` = serve-only: the
    browser frame still flows (so the daemon's ADR-0035 proxy allowlist
    learns the port) but NO frontend opens a tab, unless `fe` names one.
    `open = "<name>"` to `wglshow`/`browserview` opens it on exactly the
    frontend `sot-fe --fe <name>` names. The escape from the multi-FE
    broadcast-open, where two live clients race one shared Figure layout
    (`resize_to = :parent`).
    """
    open::Bool
    """
    The one frontend that opens the page although `open` is false: its address
    `fe@<name>`. Set by `open = "<name>"`.
    """
    fe::Union{Nothing,String}
end
BrowserView(url::AbstractString) = BrowserView(String(url), true, nothing)
BrowserView(url::AbstractString, open::Bool) = BrowserView(String(url), open, nothing)
function BrowserView(url::AbstractString, fe::AbstractString)
    isempty(fe) && throw(ArgumentError("open: name a frontend (what `sot-fe --fe` takes), or pass true/false"))
    return BrowserView(String(url), false, startswith(fe, "fe@") ? String(fe) : "fe@" * fe)
end

# Decision 0031: a BrowserView's address can carry its page's secret, and the REPL renders a returned value as text
# (a BrowserView inside a tuple, an array or a Dict included) that reaches logs and `sot-fe` output. Its display shows
# the origin only, without any userinfo; the address itself is `bv.url`.
function Base.show(io::IO, bv::BrowserView)
    # Greedy to the authority's LAST `@`: a userinfo may itself hold a raw `@`.
    m = match(r"^([A-Za-z][A-Za-z0-9+.-]*://)(?:[^/?#]*@)?([^/?#@]*)", bv.url)
    print(io, "BrowserView(", m === nothing ? "" : m[1] * m[2], "/…, open = ", bv.open)
    bv.fe === nothing || print(io, ", fe = ", repr(bv.fe))
    print(io, ")")
end

"""
    browserview(url; open = true) -> BrowserView

Convenience constructor. `return browserview(server_url)` at the end of an eval
to open `url` in the frontend's browser; `open = false` serves without opening
anywhere (see [`BrowserView`](@ref)), and `open = "<name>"` opens it only on the
frontend `sot-fe --fe <name>` names.
"""
browserview(url::AbstractString; open::Union{Bool,AbstractString} = true) = BrowserView(url, open)

# Decision 0031, the page helper: one server per REPL child, bound once on a port the OS assigns and kept for the
# child's life, so its listener never closes while its secret is valid. A secret lives exactly as long as its
# server: it is minted when the server is bound, and a server that closes takes it along, so whoever binds a
# closed port learns only a dead one.
# The one Bonito server `wglshow` binds per REPL child (see `page_server`); a re-serve replaces its app, never its
# listener. `nothing` or `(server = <Bonito server>, path = "/<32 hex>")`: the path holds the secret and lives
# exactly as long as its server. `Any`: ShipToolsRepl never loads Bonito.
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

# A Bonito server on `host`:`port` (0: the OS picks). Bonito moves a taken port to the next one with a warning; a
# pinned port that is taken throws instead. `proxy_url` is `WGL_PROXY_URL`, so the page's websocket names no origin.
function page_server(Bonito, host::String, port::Int)
    server = Base.invokelatest(Bonito.Server, host, port)
    if port != 0 && server.port != port
        try Base.invokelatest(close, server) catch end
        error("wglshow: port $port is taken")
    end
    server.proxy_url = WGL_PROXY_URL
    return server
end

# Bonito 5.x's `websocket_url` dials the page's own origin (`window.location`) when `proxy_url` is exactly "./", and
# the given origin otherwise. A remote frontend opens the page at a port of its own computer, not this server's port
# (ADR 0035, the window's half), so the page must name no origin of its own.
const WGL_PROXY_URL = "./"

# The loopback address `wglshow` serves on.
const WGL_HOST = "127.0.0.1"

# The page server `wglshow` serves from, bound once per REPL lifetime: the first call binds it (on `port`, nothing:
# any port the OS assigns; a taken port throws and publishes no server), and every later call reuses it. A different
# explicit port while it is live throws before anything changes: the listener, its secret path and its routes stay
# as they were. Restart the REPL to choose another port.
function wgl_server(Bonito, port::Union{Integer,Nothing})
    port === nothing || 1 <= port <= 65535 || throw(ArgumentError("wglshow: port must be in 1:65535"))
    page = WGL_SERVER[]
    if page === nothing
        page = (server = page_server(Bonito, WGL_HOST, port === nothing ? 0 : Int(port)),
                path = "/" * bytes2hex(rand(Random.RandomDevice(), UInt8, 16)))
        WGL_SERVER[] = page
    elseif port !== nothing && port != page.server.port
        throw(ArgumentError("wglshow: the page server is already bound to port $(page.server.port); restart the REPL to use port $port"))
    end
    return page
end

# `wglshow` renders pages Bonito's way only on the Bonito 5.1 line and later 5.x: the page handler below rebuilds the
# body of Bonito's own `apply_handler(::App, context)` with another asset server.
wgl_bonito_supported(v::VersionNumber) = v"5.1" <= v < v"6"

# The page's route: a Bonito session of its own with `NoServer`, so every script and file the page uses travels inside
# it and this port has no `/assets/` route, answered with `Referrer-Policy: no-referrer` so no request the page makes
# names its secret path. (Bonito's own app route builds its session with `HTTPAssetServer`, which registers that route
# and serves every file a session registers to any account that can compute the key from the file's path.)
function no_referrer_page(Bonito, app)
    return function (context)
        server = context.application
        connection = Base.invokelatest(Bonito.WebSocketConnection, server)
        session = Base.invokelatest(Bonito.Session, connection; asset_server = Base.invokelatest(Bonito.NoServer), title = app.title)
        body = sprint(io -> Base.invokelatest(Bonito.page_html, io, session, app))
        Base.invokelatest(Bonito.mark_displayed!, session)
        response = Base.invokelatest(Bonito.HTTPServer.html, body)
        Base.invokelatest(Bonito.HTTP.setheader, response, "Referrer-Policy" => "no-referrer")
        return response
    end
end

"""
    wglshow(fig; port = nothing, open = true) -> BrowserView

Serve an interactive WGLMakie figure over Bonito on a loopback port and return a
[`BrowserView`](@ref), so the frontend auto-opens it in the browser (ADR 0032).

`open = true` opens the figure on every attached frontend. `open = false` opens it nowhere; the frame still
flows, so the daemon's proxy allowlist learns the port. `open = "<name>"` (the name `sot-fe --fe` takes) opens
it only on that frontend. Use that when several frontends are attached: two live browser clients on one figure
race its shared layout (`resize_to = :parent`). A frontend that predates `open = "<name>"` opens nothing.
Call it as the last expression of an eval:

    using WGLMakie
    fig = surface(-10:0.4:10, -10:0.4:10, (x, y) -> sin(x) + cos(y);
                  axis = (; type = Axis3))
    wglshow(fig)

`ShipToolsRepl` carries no plotting dependency: WGLMakie/Bonito are resolved at
call time by PkgId from `Base.loaded_modules` — WGLMakie just needs to be
*loaded* in this REPL's world (directly via `using WGLMakie`, or transitively
through a package that depends on it; Bonito then comes in as WGLMakie's own
dependency). The figure is served by one Bonito server per REPL process, bound on `127.0.0.1` at a port the OS assigns and
kept for the REPL's life, at a secret path minted with that server (decision 0031). The page carries its
scripts and files inside it and its websocket sits under an unguessable session id, so another account on the box that
finds the port gets nothing: `/` and every other path answer 404. The page is sent with `Referrer-Policy: no-referrer`.
A repeat `wglshow` shows the new figure at the same address without closing the listener. Tabs still showing
an earlier figure keep it until they close. The frontend opens the page through a one-use local redirect, so the
address is never on a command line. A remote frontend reaches it through the per-URL ADR-0035 proxy. Pass
`port` (1-65535) on the first call to pin the listener. Repeated calls reuse that listener. A different port while it is live throws `ArgumentError` and leaves its page and secret unchanged; restart the REPL to choose another port. A taken first pin throws without publishing a server.

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

Needs Bonito 5.1 or a later 5.x (checked at each call); validated against WGLMakie 0.13 and Bonito 5.1 (ADR 0032).
"""
function wglshow(fig; port::Union{Integer,Nothing} = nothing, open::Union{Bool,AbstractString} = true)
    WGL = get(Base.loaded_modules, WGLMAKIE_PKGID, nothing)
    WGL === nothing && error("wglshow: WGLMakie is not loaded in this REPL — load it directly (`using WGLMakie`) or through a package that depends on it")
    # Bonito arrives as WGLMakie's dependency; require it by UUID (already loaded,
    # so this just returns the module) rather than assume the user `using`d it.
    Bonito = Base.require(Base.PkgId(
        Base.UUID("824d6782-a2ef-11e9-3a09-e5662e0c26f8"), "Bonito"))
    wgl_bonito_supported(pkgversion(Bonito)) || error("wglshow needs Bonito 5.1 or a later 5.x; this REPL loaded Bonito $(pkgversion(Bonito))")
    host = WGL_HOST
    page = wgl_server(Bonito, port)
    external = "http://$host:$(page.server.port)"
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
        listen_url = host, listen_port = page.server.port, proxy_url = WGL_PROXY_URL)
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
    # Decision 0031: the figure lives at the server's secret path; `/` answers 404. `route!` replaces the previous
    # figure in place, so the listener and the URL outlive every re-serve.
    Base.invokelatest(Bonito.route!, page.server, page.path => no_referrer_page(Bonito, app))
    # Announce at serve time, not only via the last value: a wrapper that swallows the return value would
    # otherwise leave the port unallowlisted and the page unopened.
    return announce_browserview(BrowserView(external * page.path, open))
end
