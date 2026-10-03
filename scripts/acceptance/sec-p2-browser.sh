#!/usr/bin/env bash
# Security premise P2 (Linux, a terminal in the desktop session): the owner's browser connects from the
# owner's uid. Opens the default browser once. PASS: same-account=True.
python3 - <<'EOF'
import os, socket, subprocess, time
l = socket.socket(); l.bind(("127.0.0.1", 0)); l.listen(1); port = l.getsockname()[1]
subprocess.Popen(["xdg-open", f"http://127.0.0.1:{port}/sot-owner-probe"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
l.settimeout(30); c, (_, rport) = l.accept(); t = time.perf_counter(); uid = None
for line in open("/proc/net/tcp").read().splitlines()[1:]:
    f = line.split()
    if f[1] == "0100007F:%04X" % rport and f[2] == "0100007F:%04X" % port: uid = int(f[7]); break
print(f"browser uid={uid} mine={os.geteuid()} same-account={uid == os.geteuid()} lookup={(time.perf_counter()-t)*1000:.2f}ms")
c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"); c.close()
EOF
