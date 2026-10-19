# The daemon's signal landing in the shim's own code after an eval's code ended (it lands in whatever Julia code runs):
# the shim drops it and runs the step again, so the answer is whole. Included by runtests.jl inside its testset.

# An output stream that throws an InterruptException (the daemon's signal, landing in the shim's own code) the first
# time a write holds `needle`, before the bytes are kept.
mutable struct StrayIO <: IO
    needle::String
    thrown::Bool
    bytes::Vector{UInt8}
end
StrayIO(needle) = StrayIO(needle, false, UInt8[])
function Base.unsafe_write(io::StrayIO, p::Ptr{UInt8}, n::UInt)
    chunk = unsafe_wrap(Array, p, n)
    if !io.thrown && occursin(io.needle, String(copy(chunk)))
        io.thrown = true
        throw(InterruptException())
    end
    append!(io.bytes, chunk)
    return n
end
Base.write(io::StrayIO, b::UInt8) = (push!(io.bytes, b); 1)
Base.flush(::StrayIO) = nothing


# The daemon's signal can also land in the shim's code after the eval's code ended; there it is dropped and the
# step runs again, so the answer is whole.
@testset "stream_eval_frames: a stray interrupt while it emits the answer does not lose it" begin
    for (kind, code) in (("value", () -> 42), ("error", () -> error("boom")))
        thrown = Ref(false)
        frames = Dict[]
        emit = f -> (f[:kind] == kind && !thrown[] && (thrown[] = true; throw(InterruptException()));
                     push!(frames, f))
        escaped = try
            DR.stream_eval_frames(emit) do
                code()
            end
            false
        catch e
            e isa InterruptException
        end
        @test thrown[]
        @test !escaped
        @test any(f -> f[:kind] == kind, frames)
    end
end

@testset "spawn_eval: a stray interrupt while it writes the done frame leaves the done and the res" begin
    io = StrayIO("\"done\"")
    DR.spawn_eval(emit -> nothing, io, 5, 9, "repl.eval", Dict())
    task = DR.CURRENT_EVAL[].task
    try
        wait(task)
    catch
    end
    text = String(copy(io.bytes))
    @test io.thrown
    @test occursin("\"kind\":\"done\"", text) || occursin("\"kind\": \"done\"", text)
    @test occursin("\"kind\":\"res\"", text) || occursin("\"kind\": \"res\"", text)
    @test DR.CURRENT_EVAL[] === nothing
end

@testset "stream_pipe: a stray interrupt while it emits a chunk does not lose the chunk" begin
    pipe = Pipe()
    Base.link_pipe!(pipe; reader_supports_async = true, writer_supports_async = true)
    texts = String[]
    thrown = Ref(false)
    emit = f -> (!thrown[] && (thrown[] = true; throw(InterruptException())); push!(texts, f[:text]))
    reader = @async DR.stream_pipe(pipe, emit, "stdout")
    write(pipe.in, "ab"); close(pipe.in)
    wait(reader)
    @test thrown[]
    @test join(texts) == "ab"
end
