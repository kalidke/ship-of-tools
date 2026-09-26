using ShipTools
using ConceptExplorerCore
using Documenter
using DocumenterVitepress

# ---------------------------------------------------------------------------
# Stage single-sourced pages into docs/src at build time.
#
# Several published pages have their canonical source elsewhere in the repo
# (the requirements doc, the phase-1 plan, the comm wire protocol, the host
# enrolment checklist). We COPY them in at build time rather than duplicate
# them by hand, so the published docs and the source of truth can never
# drift. The staged copies are gitignored (see .gitignore); only the
# hand-written pages are tracked under docs/src.
# ---------------------------------------------------------------------------
const SRC  = joinpath(@__DIR__, "src")
const REPO = normpath(joinpath(@__DIR__, ".."))

function stage(src, dest)
    isfile(src) || error("stage: missing source $src")
    mkpath(dirname(dest))
    cp(src, dest; force = true)
    return nothing
end

stage(joinpath(REPO, "requirements.md"),          joinpath(SRC, "design", "requirements.md"))
stage(joinpath(@__DIR__, "plan.md"),              joinpath(SRC, "design", "roadmap.md"))
stage(joinpath(REPO, "comm", "PROTOCOL.md"),      joinpath(SRC, "ref", "comm.md"))
stage(joinpath(@__DIR__, "ENROLLING-A-HOST.md"),  joinpath(SRC, "guide", "enrolling.md"))

DocMeta.setdocmeta!(ShipTools, :DocTestSetup, :(using ShipTools); recursive = true)
DocMeta.setdocmeta!(ConceptExplorerCore, :DocTestSetup, :(using ConceptExplorerCore); recursive = true)

makedocs(;
    modules = [ShipTools, ConceptExplorerCore],
    authors = "kalidke",
    sitename = "Ship of Tools",
    format = DocumenterVitepress.MarkdownVitepress(;
        repo = "github.com/kalidke/ship-of-tools",
        devbranch = "main",
        devurl = "dev",
        deploy_url = "https://kalidke.github.io/ship-of-tools",
        description = "An agentic development environment for Julia: drive many Claude Code and Codex sessions from the keyboard, with a live Julia REPL and rich previews, local or on your GPU server.",
    ),
    pages = [
        "Home" => "index.md",
        "Get started" => [
            "What it does" => "features.md",
            "Quickstart" => "start/quickstart.md",
            "Your first session" => "start/tour.md",
            "Going remote" => "start/remote.md",
            "Install details" => "start/install.md",
            "Per-machine setup" => "start/setup.md",
        ],
        "Guides" => [
            "Running several agents" => "guide/agents.md",
            "Agents messaging each other" => "guide/messaging.md",
            "Figures" => "guide/figures.md",
            "The REPL" => "guide/repl.md",
            "Previews" => "guide/previews.md",
            "A second frontend" => "guide/second-frontend.md",
            "Updating and rollback" => "guide/updating.md",
            "Enrolling a host" => "guide/enrolling.md",
            "Troubleshooting" => "guide/troubleshooting.md",
        ],
        "Concepts" => [
            "Architecture" => "guide/architecture.md",
            "Agent sessions" => "guide/orchestrator.md",
            "Sessions and persistence" => "concepts/sessions.md",
            "Work-state colours" => "concepts/work-state.md",
            "The comm relay" => "concepts/comm.md",
            "The interface" => "guide/interface.md",
            "Modes" => "guide/modes.md",
        ],
        "Extend" => [
            "Tutorial: an HDF5 preview" => "extend/hdf5.md",
            "Writing a FileType plugin" => "extend/filetype.md",
            "The dispatch ABI" => "extend/abi.md",
        ],
        "Reference" => [
            "Keybindings" => "ref/keybindings.md",
            "Configuration files" => "ref/config.md",
            "Comm protocol" => "ref/comm.md",
            "API: ConceptExplorerCore" => "ref/api-core.md",
            "API: ShipTools" => "ref/api-shiptools.md",
            "API: kernel" => "ref/api-kernel.md",
        ],
        # Planned or partly built designs, kept out of the user path; the
        # sidebar shows this group collapsed like every other.
        "Design notes" => [
            "Roadmap" => "design/roadmap.md",
            "Requirements" => "design/requirements.md",
            "Backend and sessions" => "design/backend.md",
            "Line protocol" => "design/protocol.md",
            "Frontend rendering" => "design/rendering.md",
            "The concept layer" => "guide/concept-layer.md",
            "Provenance colours" => "guide/color-coding.md",
            "Plugin discovery" => "extend/discovery.md",
            "Mode plugins" => "extend/mode.md",
        ],
        "Project" => [
            "Contributing" => "contributing.md",
            "License" => "license.md",
        ],
    ],
)

# Two checks on the built site, each for a class of mistake that builds
# cleanly and ships broken: a literal <kbd> (no such tag survives the
# markdown pipeline), and a link to a section id that does not exist (ids are
# case-sensitive, so a lowercased #fragment lands at the top of the page).
let build = joinpath(@__DIR__, "build"), site = isdir(joinpath(build, "1")) ? joinpath(build, "1") : build,
    pages = Dict{String,String}()
    for (root, _, files) in walkdir(site), file in files
        endswith(file, ".html") && (pages[joinpath(root, file)] = read(joinpath(root, file), String))
    end
    ids = Dict(p => Set(m[1] for m in eachmatch(r"\bid=\"([^\"]+)\"", t)) for (p, t) in pages)
    bad = String[]
    for (p, t) in pages
        occursin("&lt;kbd&gt;", t) && push!(bad, "literal <kbd> in $p")
        for m in eachmatch(r"href=\"([^\"#:]*)#([^\"]+)\"", t)
            path, frag = m[1], m[2]
            target = isempty(path) ? p :
                     normpath(startswith(path, "/") ? joinpath(site, lstrip(path, '/')) : joinpath(dirname(p), path))
            i = findfirst(c -> haskey(pages, c), [target, target * ".html", joinpath(target, "index.html")])
            i === nothing && continue   # not a page of this site (or under a deploy base path)
            target = [target, target * ".html", joinpath(target, "index.html")][i]
            frag in ids[target] && continue
            push!(bad, "$(relpath(p, site)): link to missing section #$frag in $(relpath(target, site))")
        end
    end
    isempty(bad) || error("built site checks failed:\n  " * join(unique(bad), "\n  "))
end

# DocumenterVitepress decides for itself whether this run may deploy (a CI
# push to main or a tag); a local build never deploys.
DocumenterVitepress.deploydocs(;
    repo = "github.com/kalidke/ship-of-tools",
    devbranch = "main",
    push_preview = false,
)
