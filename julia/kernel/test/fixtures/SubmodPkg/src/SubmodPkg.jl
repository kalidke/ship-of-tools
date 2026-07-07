"""
    SubmodPkg

Docstringed top-level module — reproduces the `K"doc"[string, module]` shape
that previously hid a module's members from the scan walk.
"""
module SubmodPkg

include("sub/Sub.jl")

struct TopType
    a::Int
end

topfun() = 1

end # module
