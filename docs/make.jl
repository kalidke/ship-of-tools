using ShipTools
using ConceptExplorerCore
using Documenter

# ---------------------------------------------------------------------------
# Stage single-sourced pages into docs/src at build time.
#
# Several published pages have their canonical source elsewhere in the repo
# (the requirements doc, the phase-1 plan, the comm wire protocol). We COPY
# them in at build time rather than duplicate them by hand, so the published
# docs and the source of truth can never drift. The staged copies are
# gitignored (see .gitignore); only the hand-written pages are tracked under
# docs/src.
# ---------------------------------------------------------------------------
const SRC  = joinpath(@__DIR__, "src")
const REPO = normpath(joinpath(@__DIR__, ".."))

function stage(src, dest)
    isfile(src) || error("stage: missing source $src")
    mkpath(dirname(dest))
    cp(src, dest; force = true)
    return nothing
end

stage(joinpath(REPO, "requirements.md"),     joinpath(SRC, "design", "requirements.md"))
stage(joinpath(@__DIR__, "plan.md"),         joinpath(SRC, "design", "roadmap.md"))
stage(joinpath(REPO, "comm", "PROTOCOL.md"), joinpath(SRC, "ref", "comm.md"))

DocMeta.setdocmeta!(ShipTools, :DocTestSetup, :(using ShipTools); recursive = true)
DocMeta.setdocmeta!(ConceptExplorerCore, :DocTestSetup, :(using ConceptExplorerCore); recursive = true)

makedocs(;
    modules = [ShipTools, ConceptExplorerCore],
    authors = "kalidke",
    sitename = "Ship of Tools",
    format = Documenter.HTML(;
        canonical = "https://kalidke.github.io/ship-of-tools",
        edit_link = "main",
        assets = ["assets/favicon.ico", "assets/custom.css"],
    ),
    # ------------------------------------------------------------------------
    # NAV SCOPE: the published site is a lean new-user front. Everything else
    # (feature tour, full user guide, extending, reference, design/internals)
    # stays on disk under docs/src — it is the corpus the in-app help agent
    # (the frontend Terminal Claude Code session) reads directly from the
    # repo — but is deliberately left out of `pages` below.
    #
    # Documenter (1.17, the version this was validated against) still BUILDS
    # every .md file it finds under docs/src regardless of `pages` — `pages`
    # only controls what appears in the nav — so leaving those files off this
    # list does not break the build or orphan their links; it just keeps them
    # out of the visible navigation. `pagesonly` (which would drop them from
    # the build entirely) is deliberately left at its default `false`.
    # ------------------------------------------------------------------------
    pages = [
        "Home" => "index.md",
        "Getting Started" => [
            "Quickstart" => "start/quickstart.md",
            "Install Details" => "start/install.md",
            "First Session Tour" => "start/tour.md",
        ],
        "The Interface" => "guide/panes/index.md",
    ],
)

deploydocs(;
    repo = "github.com/kalidke/ship-of-tools",
    devbranch = "main",
)
