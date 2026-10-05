# The options of the daemon's Pluto session, in one place so the test (test/runtests.jl) configures exactly what
# start.jl runs.

"""
    configure_session!(session, host, port)

Configure `session` (a `Pluto.ServerSession`) as the daemon's Pluto server: loopback only, no browser, and the two
locks of ADR 0049 (User isolation): every request to Pluto's page carries the session secret, and every notebook runs in
a Distributed worker.
"""
function configure_session!(session, host::AbstractString, port::Integer)
    session.options.server.host = host
    session.options.server.port = port
    session.options.server.launch_browser = false
    session.options.server.show_file_system = false
    session.options.server.disable_writing_notebook_files = false
    # Access-gated (security review): any local user on this shared host could
    # otherwise reach the Pluto UI and run code as the daemon's owner — an RCE
    # as bad as an open protocol port. `require_secret_for_access = true` makes
    # every request need `session.secret` (URL query param or the cookie Pluto
    # sets after the first authenticated hit); the `URL` line in start.jl appends
    # it, so the `o`-opens-Pluto flow keeps working with no frontend change.
    session.options.security.require_secret_for_open_links = true
    session.options.security.require_secret_for_access = true
    session.options.security.warn_about_untrusted_code = true
    # The notebook workers are the other door. Pluto 0.20's default worker (Malt) listens on 127.0.0.1 at a port any
    # account can compute from the worker's pid, accepts the FIRST connection with no handshake and then runs whatever
    # function that connection sends: another account that connects before Pluto does runs code as this user. A
    # Distributed worker listens on loopback too (LocalManager binds 127.0.0.1) but reads a cluster cookie from its
    # stdin, never from its command line, and closes a connection whose header carries another cookie before it reads a
    # message. The cost: on Windows Pluto cannot stop a running cell in this mode (it says so; owner: 0.6.7).
    session.options.evaluation.workspace_use_distributed_stdlib = true
    return session
end
