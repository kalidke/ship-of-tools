# An output stream for `ShipToolsRepl.serve` that reproduces the order a real pipe allows: the client reads an answer
# while the answering task still waits on its write, and sends its next request at once. The moment the terminal res
# of request `trigger` has been written, `next()` hands the dispatch loop that next request, and the loop runs before
# the answering task resumes.
mutable struct AnswerThenNext <: IO
    out::Base.BufferStream
    pending::Vector{UInt8}
    trigger::Int
    next::Function
    fired::Bool
end

AnswerThenNext(out::Base.BufferStream, trigger::Integer, next::Function) =
    AnswerThenNext(out, UInt8[], trigger, next, false)

function Base.unsafe_write(io::AnswerThenNext, p::Ptr{UInt8}, n::UInt)
    append!(io.pending, unsafe_wrap(Array, p, n))
    return unsafe_write(io.out, p, n)
end

function Base.write(io::AnswerThenNext, b::UInt8)
    push!(io.pending, b)
    return write(io.out, b)
end

function Base.flush(io::AnswerThenNext)
    flush(io.out)
    cut = findlast(==(UInt8('\n')), io.pending)
    cut === nothing && return nothing
    lines = split(String(io.pending[1:cut]), '\n'; keepempty = false)
    deleteat!(io.pending, 1:cut)
    for line in lines
        env = ShipToolsRepl.json_read(line)
        if !io.fired && get(env, :kind, "") == "res" && get(env, :id, 0) == io.trigger
            io.fired = true
            io.next()
            # One thread, cooperative tasks: the dispatch loop, now runnable, reads the request and acts on it
            # before this task, which is inside the answer's write, runs again.
            for _ in 1:100
                yield()
            end
        end
    end
    return nothing
end
