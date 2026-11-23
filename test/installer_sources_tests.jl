@testset "the installer finds every manifest package in the selected depot" begin
    mktempdir() do root
        project = mkpath(joinpath(root, "env"))
        depot = mkpath(joinpath(root, "depot"))
        second_depot = mkpath(joinpath(root, "second-depot"))
        uuid = "415d0215-7570-497b-989c-ed344c5ad569"
        tree = repeat("1", 40)
        write(joinpath(project, "Project.toml"), "[deps]\nDirect = \"$uuid\"\n")
        checker = joinpath(@__DIR__, "..", "rust", "backend", "src", "install", "check_sources.jl")
        # The check runs in a fresh Julia with no package cache or project code loaded.
        probe = """
            loaded = String[]
            push!(Base.package_callbacks, id -> push!(loaded, id.name))
            include(ARGS[1])
            isempty(loaded) || error("the source check loaded packages: ", join(loaded, ", "))
            """
        cmd = addenv(`$(Base.julia_cmd()) --startup-file=no --compiled-modules=existing --project=$project -e $probe $checker`,
            "JULIA_DEPOT_PATH" => join((depot, second_depot), Sys.iswindows() ? ";" : ":"), "JULIA_PKG_OFFLINE" => "true")
        function check()
            out, err = IOBuffer(), IOBuffer()
            proc = run(pipeline(ignorestatus(cmd), stdout=out, stderr=err))
            (success(proc), String(take!(out)), String(take!(err)))
        end
        ok, _, err = check()
        @test !ok && occursin("missing manifest", err)
        write(joinpath(project, "Manifest.toml"), """
            manifest_format = "2.0"
            [[deps.Direct]]
            deps = ["Leaf", "Random"]
            path = "direct"
            uuid = "$uuid"
            [[deps.Leaf]]
            git-tree-sha1 = "$tree"
            uuid = "f25fc660-da26-4755-8557-e1e318771656"
            version = "1.0.0"
            [[deps.Random]]
            uuid = "9a3f8284-a2c9-5f02-9a11-845980a1fd5c"
            """)
        write(joinpath(mkpath(joinpath(project, "direct", "src")), "Direct.jl"), "error(\"loaded Direct\")")
        slug = Base.version_slug(Base.UUID("f25fc660-da26-4755-8557-e1e318771656"), Base.SHA1(tree))
        src = mkpath(joinpath(second_depot, "packages", "Leaf", slug, "src"))
        ok, _, err = check()
        @test !ok && "SOT-SOURCE-CHECK: missing source for Leaf" in eachline(IOBuffer(err))
        write(joinpath(src, "Leaf.jl"), "error(\"loaded Leaf\")")
        @test check() == (true, "sources present", "")
        manifest = joinpath(project, "Manifest.toml")
        original = read(manifest, String)
        # A name-only dependency needs one match. A UUID-qualified dependency needs that particular entry.
        for refs in ("[\"Absent\"]", "{ Leaf = \"$uuid\" }")
            write(manifest, replace(original, """["Leaf", "Random"]""" => refs))
            ok, _, err = check()
            @test !ok && occursin("for Direct", err)
        end
        # The loader also accepts v1 manifests. A compatibility entry listed in weakdeps is optional.
        write(manifest, replace(original, "manifest_format = \"2.0\"\n" => "", "[[deps." => "[["))
        open(joinpath(project, "Project.toml"), "a") do io
            optional = "4cc7eff0-f141-4dcb-ab94-e549f9a623fd"
            println(io, "Optional = \"$optional\"\n[weakdeps]\nOptional = \"$optional\"")
        end
        @test check() == (true, "sources present", "")
        write(joinpath(project, "Project.toml"), "[deps]\nRenamed = \"$uuid\"\n")
        ok, _, err = check()
        @test !ok && occursin("manifest is missing Renamed", err)
        @test all(d -> !isdir(joinpath(d, "compiled")), (depot, second_depot))
    end
end
