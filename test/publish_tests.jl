# Tests for src/publish.jl: install_file and _install_files publishing, staging names and marker reaping.

@testset "install_file: copy-then-rename" begin
    # install_file replaced every `cp(src, dst; force = true)` file-copy
    # site because that form removes dst BEFORE copying: a copy that then
    # fails leaves nothing at dst, not merely an un-updated file. This is
    # exactly how comm-relay.sh vanished from a live ~/.sot-comm/bin while
    # every other script updated normally (a Windows FE box, 2026-09-04).
    mktempdir() do dir
        # Plain success: dst gets src's content, no leftover temp file.
        src = joinpath(dir, "src.txt")
        write(src, "hello")
        dst = joinpath(dir, "dst.txt")
        ShipTools.install_file(src, dst)
        @test read(dst, String) == "hello"
        @test !any(n -> occursin(".tmp-", n), readdir(dir))

        # Failure with a PRE-EXISTING dst (the field scenario: an update
        # over a working install): the old dst must survive untouched,
        # and no temp file should linger.
        missing_src = joinpath(dir, "does-not-exist.txt")
        old_dst = joinpath(dir, "old.txt")
        write(old_dst, "OLD CONTENT")
        err = try
            ShipTools.install_file(missing_src, old_dst)
            nothing
        catch e
            e
        end
        @test err isa ErrorException
        @test read(old_dst, String) == "OLD CONTENT"
        @test !isfile(old_dst * ".tmp")
        # The error's FIRST line names dst and the underlying cause: a
        # launcher that only surfaces the tail of a crash's stderr must
        # still see the useful part, not just the last stack frame.
        firstline = split(err.msg, '\n'; limit = 2)[1]
        @test occursin(old_dst, firstline)
        @test occursin("no such file", lowercase(firstline)) ||
              occursin("enoent", lowercase(firstline))

        # Failure with NO prior dst (a first install of that file): dst
        # must stay absent, not half-written.
        fresh_dst = joinpath(dir, "fresh.txt")
        @test !isfile(fresh_dst)
        @test_throws ErrorException ShipTools.install_file(missing_src, fresh_dst)
        @test !isfile(fresh_dst)
        @test !isfile(fresh_dst * ".tmp")

        # A genuine permission failure (closer to the field defect than a
        # missing source) tells the same story. Skipped if running as
        # root, where a mode of 0 still reads fine.
        unreadable_src = joinpath(dir, "unreadable.txt")
        write(unreadable_src, "secret")
        chmod(unreadable_src, 0o000)
        can_still_read = try
            read(unreadable_src, String)
            true
        catch
            false
        end
        if !can_still_read
            write(old_dst, "OLD CONTENT 2")
            err2 = try
                ShipTools.install_file(unreadable_src, old_dst)
                nothing
            catch e
                e
            end
            @test err2 isa ErrorException
            @test read(old_dst, String) == "OLD CONTENT 2"
            fl2 = split(err2.msg, '\n'; limit = 2)[1]
            @test occursin(old_dst, fl2)
            @test occursin("denied", lowercase(fl2)) || occursin("eacces", lowercase(fl2))
        end
        chmod(unreadable_src, 0o644)  # let mktempdir clean up

        # A genuine RENAME failure (not a copy failure): src copies to
        # the temp path fine, but the publish step can't land — modeled
        # here by a pre-existing DIRECTORY at dst, which a file rename
        # can never replace. This is the failure mode that mattered most:
        # `mv(tmp, dst; force = true)` (what install_file used to call)
        # falls back to deleting dst and retrying on a failed plain
        # rename, so a stubborn destination — a live process still has
        # `comm-relay.sh` open, was the field case — got DELETED even
        # though the replace ultimately failed too. A bare rename must
        # never delete dst on failure.
        blocked_dst = joinpath(dir, "blocked")
        mkpath(blocked_dst)
        write(joinpath(blocked_dst, "marker.txt"), "keepme")
        rename_src = joinpath(dir, "new-content.txt")
        write(rename_src, "new content")
        err3 = try
            ShipTools.install_file(rename_src, blocked_dst)
            nothing
        catch e
            e
        end
        @test err3 isa ErrorException
        @test isdir(blocked_dst)  # untouched — not deleted, not replaced
        @test read(joinpath(blocked_dst, "marker.txt"), String) == "keepme"
        @test !isfile(blocked_dst * ".tmp")
        fl3 = split(err3.msg, '\n'; limit = 2)[1]
        @test occursin(blocked_dst, fl3)
    end
end

@testset "install_file: a refused FILE destination is moved aside, never deleted" begin
    # Windows refuses a rename over a file another process holds open.
    # Modeled through the
    # `rename` seam: the first rename onto dst is refused, everything
    # else behaves. The old file must survive under an aside name (the
    # holder keeps it), dst must carry the new bytes, and a later
    # install prunes the aside copy.
    mktempdir() do dir
        src = joinpath(dir, "new.sh"); write(src, "NEW")
        dst = joinpath(dir, "held.sh"); write(dst, "OLD")
        refused = Ref(true)
        fake_rename(a, b) = begin
            if refused[] && b == dst && occursin(".tmp-", a)
                refused[] = false
                error("rename($a, $b): permission denied (EACCES)")
            end
            Base.Filesystem.rename(a, b)
        end
        ShipTools.install_file(src, dst; rename = fake_rename)
        @test read(dst, String) == "NEW"
        asides = filter(n -> startswith(n, "held.sh.stale-"), readdir(dir))
        @test length(asides) == 1
        @test read(joinpath(dir, asides[1]), String) == "OLD"
        @test !any(n -> occursin(".tmp-", n), readdir(dir))
        # Reaping moved INTO install_file's success path: the aside is
        # removed by the next successful replace of the same name, so the
        # real path is what gets asserted, not a helper called by hand.
        write(src, "NEWER")
        ShipTools.install_file(src, dst)
        @test read(dst, String) == "NEWER"
        @test !any(n -> occursin(".stale-", n), readdir(dir))

        # A refusal that persists even after the aside: the old file is
        # put back and the error names dst.
        write(dst, "OLD2")
        # Refuse every PUBLISH onto dst (a .tmp source); the restore of
        # the aside copy targets a now-free name and goes through, as it
        # would on Windows.
        always_refuse(a, b) = (b == dst && occursin(".tmp-", a)) ? error("still refused") : Base.Filesystem.rename(a, b)
        err = try
            ShipTools.install_file(src, dst; rename = always_refuse)
            nothing
        catch e
            e
        end
        @test err isa ErrorException
        @test occursin(dst, err.msg)
        @test read(dst, String) == "OLD2"
        @test !any(n -> occursin(".tmp-", n), readdir(dir))
    end
end

@testset "_check_installed" begin
    # Contract: returns a list of problem descriptions, never throws —
    # callers (_install_files) fold it into one combined report.
    mktempdir() do dir
        write(joinpath(dir, "present.sh"), "x")

        probs = ShipTools._check_installed(dir, ["present.sh", "missing.sh"])
        @test any(occursin("missing.sh", p) for p in probs)
        @test any(occursin(dir, p) for p in probs)

        probs2 = ShipTools._check_installed(dir, ["present.sh"]; executable = Returns(true))
        @test any(occursin("present.sh", p) for p in probs2)

        chmod(joinpath(dir, "present.sh"), 0o755)
        @test isempty(ShipTools._check_installed(dir, ["present.sh"]; executable = Returns(true)))
    end
end

@testset "_install_files: an identical destination is current even when it cannot be replaced" begin
    mktempdir() do tmp
        srcdir = joinpath(tmp, "src"); dstdir = joinpath(tmp, "dst")
        mkpath(srcdir); mkpath(dstdir)
        write(joinpath(srcdir, "same.sh"), "#!/bin/sh\necho same\n")
        write(joinpath(dstdir, "same.sh"), "#!/bin/sh\necho same\n")
        chmod(joinpath(dstdir, "same.sh"), 0o755)
        # Make the destination directory read-only so a replace would fail.
        chmod(dstdir, 0o555)
        try
            @test ShipTools._install_files(srcdir, dstdir, ["same.sh"]; executable = endswith(".sh")) === nothing
        finally
            chmod(dstdir, 0o755)
        end
    end
end

@testset "_install_files: continues past a locked destination, reports it, updates the rest" begin
    # The property the coordinator's field trace demanded: ONE destination
    # a live process still has open (comm-relay.sh, observed) must not
    # block updating the other N-1 files in the same directory, and the
    # final report must name exactly the one that stayed stale — never
    # silently, never by aborting everything else.
    mktempdir() do base
        srcdir = joinpath(base, "src")
        dstdir = joinpath(base, "dst")
        mkpath(srcdir)
        mkpath(dstdir)
        names = ["f$(lpad(i, 2, '0')).sh" for i in 1:12]
        stuck_name = names[11]
        for (i, name) in enumerate(names)
            write(joinpath(srcdir, name), "NEW-$i")
            if name == stuck_name
                # A directory in its place: the rename step can never
                # replace it, modeling a locked/in-use destination.
                mkpath(joinpath(dstdir, name))
                write(joinpath(dstdir, name, "marker.txt"), "keepme")
            else
                write(joinpath(dstdir, name), "OLD-$i")  # a prior install
            end
        end

        err = try
            ShipTools._install_files(srcdir, dstdir, names; executable = endswith(".sh"))
            nothing
        catch e
            e
        end

        @test err isa ErrorException
        @test occursin(stuck_name, err.msg)

        # Every OTHER file updated — including ones AFTER the stuck one,
        # proving the loop did not stop early.
        for i in vcat(1:10, 12)
            @test read(joinpath(dstdir, names[i]), String) == "NEW-$i"
            @test Sys.isexecutable(joinpath(dstdir, names[i]))
        end
        # The stuck one is untouched — old "install" (the directory)
        # survives exactly as it was, not deleted, not half-replaced.
        @test isdir(joinpath(dstdir, stuck_name))
        @test read(joinpath(dstdir, stuck_name, "marker.txt"), String) == "keepme"
    end
end

@testset "_tmp_name: two live copies never share a staging path" begin
    # The load-bearing guard for the concurrency fix: a FIXED staging name
    # lets one run's copy interleave with another's rename and publish a
    # torn file under a name that then passes every existence check.
    dst = joinpath("/a", "b", "c.txt")
    a = ShipTools._tmp_name(dst)
    b = ShipTools._tmp_name(dst)
    @test a != b
    # A sibling, because the publish is a rename and a rename is atomic
    # only within one filesystem.
    @test dirname(a) == dirname(dst)
    @test a != dst * ".tmp"
    @test occursin(".tmp-", a)
end

@testset "install_file: a concurrent writer never publishes a torn file" begin
    mktempdir() do base
        dst = joinpath(base, "dst.bin")
        s1 = joinpath(base, "s1.bin"); s2 = joinpath(base, "s2.bin")
        write(s1, repeat("A", 1_000_000)); write(s2, repeat("B", 1_000_000))
        write(dst, "old")
        if Threads.nthreads() > 1
            for _ in 1:5
                t1 = Threads.@spawn ShipTools.install_file(s1, dst)
                t2 = Threads.@spawn ShipTools.install_file(s2, dst)
                for t in (t1, t2)
                    try; fetch(t); catch; end
                end
                got = read(dst, String)
                # Entirely one source or entirely the other — never mixed,
                # never truncated, never missing.
                @test got == read(s1, String) || got == read(s2, String)
            end
        else
            ShipTools.install_file(s1, dst)
            @test read(dst, String) == read(s1, String)
        end
    end
end

if Sys.isunix()
    @testset "install_file: marker reaping is owner-aware" begin
        mktempdir() do base
            dst = joinpath(base, "f.txt"); src = joinpath(base, "src.txt")
            write(dst, "OLD"); write(src, "NEW")
            # A certainly-dead pid: the child reports its own, then exits.
            proc = open(`sh -c "echo \$\$"`)
            deadpid = parse(Int, strip(read(proc, String)))
            wait(proc)
            tag = ShipTools._host_tag()
            mine_dead = joinpath(base, "f.txt.tmp-$tag-$deadpid-0001")
            foreign_live = joinpath(base, "f.txt.tmp-x$tag-$(getpid())-0002")
            aside = joinpath(base, "f.txt.stale-deadbeef")
            for f in (mine_dead, foreign_live, aside); write(f, "x"); end
            ShipTools.install_file(src, dst)
            @test read(dst, String) == "NEW"
            @test !isfile(mine_dead)      # this host, pid gone
            @test isfile(foreign_live)    # another host's pid means nothing here
            @test !isfile(aside)
        end
    end
end
