module Sub

# Members defined directly here AND in include()d files must all attribute to
# Sub at runtime — the scan must carry the enclosing-module context across the
# include boundary.
include("types.jl")

subfun() = 2

end # module
