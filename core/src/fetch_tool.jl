# Fetches Julia's copy of an external tool (a JLL package) into a folder of its own and records where its programs are.
# ConceptExplorerCore.find_tools starts it as a child of the kernel when a preview finds the tool nowhere:
#   julia --project=<dir> fetch_tool.jl <dir> <jll> <uuid> <compat> <bound seconds> <program>...
# It touches only <dir> and the user's own depot (Pkg downloads the artifacts there), loads nothing of the kernel, holds
# <dir>/fetch.lock while it runs (a second fetch finds the lock held and ends at once), and ends within the bound. Its
# outcome is a file: copy.txt on success, failed.txt with one line of fixed words otherwise. Pkg's own messages go to
# the log the parent opened (stdout and stderr), never into a file a person's note reads.

module FetchTool

using FileWatching.Pidfile: trymkpidlock
using Pkg

# Downloads the JLL and records its programs; returns nothing, or the words of a failure.
function install(dir, jll, uuid, compat, programs)
    write(joinpath(dir, "Project.toml"), "[deps]\n$jll = \"$uuid\"\n\n[compat]\n$jll = \"$compat\"\n")
    try
        Pkg.instantiate()
    catch e
        @error "instantiate failed" exception = (e, catch_backtrace())
        return "the download or install failed"
    end
    mod = try
        Base.require(Base.PkgId(Base.UUID(uuid), jll))
    catch e
        @error "load failed" exception = (e, catch_backtrace())
        return "the downloaded copy would not load"
    end
    Base.invokelatest(mod.is_available) || return "no copy is built for this platform"
    tmp = joinpath(dir, "copy.tmp")
    open(tmp, "w") do io
        println(io, "libpath_env=", mod.JLLWrappers.LIBPATH_env)
        println(io, "libpath=", Base.invokelatest(() -> mod.LIBPATH[]))
        for p in programs
            println(io, p, "=", Base.invokelatest(getproperty(mod, Symbol("get_", p, "_path"))))
        end
    end
    mv(tmp, joinpath(dir, "copy.txt"); force = true)
    nothing
end

function main(args; install = install)
    dir, jll, uuid, compat, bound = args[1:5]
    lock = trymkpidlock(joinpath(dir, "fetch.lock"); stale_age = 2, refresh = 0.5)
    lock === false && return
    failed = joinpath(dir, "failed.txt")
    rm(failed; force = true)
    function fail(words)
        write(failed, words * "\n")
        close(lock)
        exit(1)
    end
    timer = Timer(_ -> fail("it did not finish within $bound s"), parse(Int, bound))
    words = try
        install(dir, jll, uuid, compat, args[6:end])
    catch e
        @error "fetch failed" exception = (e, catch_backtrace())
        "the fetch failed"
    end
    close(timer)
    words === nothing ? close(lock) : fail(words)
end

end # module

abspath(PROGRAM_FILE) == (@__FILE__) && FetchTool.main(ARGS)
