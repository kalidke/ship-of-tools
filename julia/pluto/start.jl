using Pluto
using Sockets

const HOST = "127.0.0.1"
const PORT = 1234

session = Pluto.ServerSession()
session.options.server.host = HOST
session.options.server.port = PORT
session.options.server.launch_browser = false
session.options.server.show_file_system = false
session.options.server.disable_writing_notebook_files = false
session.options.security.require_secret_for_open_links = false
# Access-gated (security review): any local user on this shared host could
# otherwise reach the Pluto UI and run code as the daemon's owner — an RCE
# as bad as an open protocol port. `require_secret_for_access = true` makes
# every request need `session.secret` (URL query param or the cookie Pluto
# sets after the first authenticated hit); the `URL` line below appends it,
# so the `o`-opens-Pluto flow keeps working with no frontend change.
# `require_secret_for_open_links` can stay false: once access itself is
# gated, `auth_required` OSes it regardless (see Pluto's Authentication.jl),
# so it's a no-op here, not a second hole.
session.options.security.require_secret_for_access = true
session.options.security.warn_about_untrusted_code = true

server_task = Pluto.run!(session)

# Probe the port until Pluto's HTTP listener is actually bound.
let deadline = time() + 60.0
    while time() < deadline
        try
            sock = Sockets.connect(HOST, PORT)
            close(sock)
            break
        catch
            sleep(0.1)
        end
    end
end

println(stdout, "READY http://$(HOST):$(PORT)")
flush(stdout)

# Service loop: read OPEN <abspath> requests on stdin, write URL <url> or
# ERR <msg> on stdout. Stays alive until stdin closes.
for line in eachline(stdin)
    line = strip(line)
    isempty(line) && continue
    if startswith(line, "OPEN ")
        path = String(line[6:end])
        try
            nb = Pluto.SessionActions.open(session, path; run_async=true)
            # `session.secret` is the same query-param secret
            # `require_secret_for_access` now demands on every request; embed
            # it here rather than in the Rust supervisor since Julia already
            # holds it and builds this URL.
            url = "http://$(HOST):$(PORT)/edit?id=$(nb.notebook_id)&secret=$(session.secret)"
            println(stdout, "URL $url")
            flush(stdout)
        catch e
            msg = sprint(showerror, e)
            # Collapse newlines so the single-line wire stays one line.
            msg = replace(msg, '\n' => ' ')
            println(stdout, "ERR $msg")
            flush(stdout)
        end
    else
        println(stdout, "ERR unknown command: $line")
        flush(stdout)
    end
end
