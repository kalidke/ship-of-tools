# Regression test for the project.scan module-nav nesting fix.
#   julia --project=julia/kernel julia/kernel/test/scan_nesting.jl
#
# Reproduces the MicroscopePSFs/Zernike bug in miniature: a *docstringed*
# package module include()s a submodule whose own members live in further
# include()d files. Before the fix, those members flattened under the package
# and the submodule came up empty (two bugs: lost include→module context, and a
# docstring wrapper hiding the module body from the descent).

using ShipToolsKernel
const KK = ShipToolsKernel

here = @__DIR__
entry = joinpath(here, "fixtures", "SubmodPkg", "src", "SubmodPkg.jl")
@assert isfile(entry) "fixture missing: $entry"

defs = KK.scan_project_defs(entry)
file_defs = Dict{String,Vector{Dict}}()
for d in defs
    push!(get!(() -> Dict[], file_defs, d[:file]), d)
end
modules = KK.build_module_tree(file_defs, entry, "SubmodPkg")

mnames(xs) = sort([x[:name] for x in xs])

@assert mnames(modules) == ["SubmodPkg"] "top-level modules wrong: $(mnames(modules))"
pkg = first(modules)

# docstringed package module IS descended (module_body_node unwrap)
@assert "TopType" in mnames(pkg[:types]) "TopType missing — docstring-unwrap regression"
@assert "topfun" in mnames(pkg[:functions]) "topfun missing from package"

# submodule nested, not flattened
@assert "Sub" in mnames(pkg[:submodules]) "Sub not nested under SubmodPkg"
sub = first(filter(s -> s[:name] == "Sub", pkg[:submodules]))

# Sub's members — inline (subfun) AND from include()d types.jl — attribute to Sub
@assert "SubType" in mnames(sub[:types]) "SubType not under Sub"
@assert "AbstractSub" in mnames(sub[:types]) "AbstractSub not under Sub"
@assert "subfun" in mnames(sub[:functions]) "subfun not under Sub"
@assert "typesfun" in mnames(sub[:functions]) "typesfun (from include()d types.jl) not under Sub"

# NOT flattened: Sub's members must not appear under the package
@assert !("SubType" in mnames(pkg[:types])) "SubType leaked flat under SubmodPkg"
@assert !("typesfun" in mnames(pkg[:functions])) "typesfun leaked flat under SubmodPkg"

# every def physically in types.jl must carry parent=Sub
tjl = abspath(joinpath(here, "fixtures", "SubmodPkg", "src", "sub", "types.jl"))
for d in get(file_defs, tjl, Dict[])
    @assert get(d, :parent, nothing) == "Sub" "$(d[:name]) parent=$(get(d, :parent, nothing)), want Sub"
end

println("scan_nesting: PASS — submodule populated, nested, not flattened; docstringed module descended.")
