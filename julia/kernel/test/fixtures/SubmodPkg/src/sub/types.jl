# Top-level defs in an include()d file. At runtime these belong to the module
# that include()s this file (Sub), NOT to the package — the regression is that
# they used to flatten under the package because each file was parsed standalone.

struct SubType
    x::Int
    y::Float64
end

abstract type AbstractSub end

typesfun() = 3
