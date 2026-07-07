# Ship of Tools `r` demo — run this one FIRST, with `r` in NavTree.
#
# `r` resets the workspace REPL to a fresh `julia --project=<closest Project.toml>`
# and then include()s this file. It is fully self-contained: it defines some
# globals and helpers in Main and prints a confirmation, so it works from a
# clean REPL with no prior state.
#
# After this runs, the REPL holds `GREETING`, `samples`, and `summarize` —
# which is exactly what 02_use_setup.jl reaches for when you run it with `R`.

const GREETING = "set up by 01_setup.jl"

# A little data the next file will consume.
samples = collect(1:10)

"""
    summarize(xs)

Return a NamedTuple of basic stats. Defined here so a follow-up file
run with `R` can call it against the existing REPL session.
"""
function summarize(xs)
    n = length(xs)
    return (n = n, total = sum(xs), mean = sum(xs) / n)
end

println("[01_setup] ", GREETING)
println("[01_setup] samples = ", samples)
println("[01_setup] summarize(samples) = ", summarize(samples))
println("[01_setup] ready — now run 02_use_setup.jl with R")
