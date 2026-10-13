# Pluto's environment prepared once in the depot, with one notebook run so its worker side compiles too, before the
# daemon lifetime harness runs: its Pluto cases then get this manifest (SOT_L2_PLUTO_MANIFEST) and resolve, install and
# compile nothing. Run with --project=julia/pluto and a copy of fixtures/notebook.jl, by rust.yml's Linux job and by
# rc-gate.sh's job_lifetime.
using Pkg
Pkg.instantiate()
using Pluto
include(joinpath(dirname(Base.active_project()), "session_options.jl"))
session = configure_session!(Pluto.ServerSession(), "127.0.0.1", 1234)
notebook = Pluto.SessionActions.open(session, ARGS[1]; run_async = false)
ran = notebook.cells[1].output.body == "2"
Pluto.SessionActions.shutdown(session, notebook; keep_in_session = false, async = false)
ran || error("the warm-up notebook did not run")
