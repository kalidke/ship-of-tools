# Ship of Tools `R` demo — run this one SECOND, with `R` in NavTree.
#
# `R` include()s this file into the EXISTING REPL without resetting it. This
# file deliberately depends on state established by 01_setup.jl — `GREETING`,
# `samples`, and `summarize`. Run it with `R` after running 01_setup.jl with
# `r` and it picks up right where that left off.
#
# Run it with `r` (or `R` on a fresh REPL) and it throws UndefVarError on
# `GREETING` — proof that it is consuming live session state, not re-deriving it.

@assert isdefined(Main, :GREETING) "run 01_setup.jl with `r` first, then this with `R`"

println("[02_use] inherited GREETING = ", GREETING)

# Extend the data the previous file left in the session.
append!(samples, [100, 200, 300])

stats = summarize(samples)
println("[02_use] samples now = ", samples)
println("[02_use] summarize(samples) = ", stats)
println("[02_use] mean moved to ", round(stats.mean; digits = 2), " after appending")
