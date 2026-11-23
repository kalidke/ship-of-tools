# Resolve sources only from the selected project and this Julia's standard library.
empty!(LOAD_PATH)
append!(LOAD_PATH, ("@", "@stdlib"))
function source_failure(reason)
    println(stderr, "\nSOT-SOURCE-CHECK: ", reason)
    exit(1)
end
project = Base.active_project()
isfile(project) || source_failure("missing Project.toml")
file = Base.project_file_manifest_path(project)
isnothing(file) && source_failure("missing manifest")
manifest = Base.get_deps(Base.parsed_toml(file))
has_entry(name, uuid) = any(entry -> Base.UUID(entry["uuid"]) == Base.UUID(uuid), get(manifest, name, ()))
settings = Base.parsed_toml(project)
weakdeps = get(settings, "weakdeps", Dict())
for (name, uuid) in get(settings, "deps", ())
    haskey(weakdeps, name) && Base.UUID(weakdeps[name]) == Base.UUID(uuid) && continue
    has_entry(name, uuid) || source_failure("manifest is missing $name")
end
for (name, entries) in manifest, entry in entries
    for dep in get(entry, "deps", ())
        if dep isa Pair
            has_entry(dep...) || source_failure("manifest is missing $(first(dep)) for $name")
        else
            length(get(manifest, dep, ())) == 1 || source_failure("manifest must identify one $dep for $name")
        end
    end
    id = Base.PkgId(Base.UUID(entry["uuid"]), name)
    isnothing(Base.locate_package(id)) && source_failure("missing source for $name")
end
print("sources present")
