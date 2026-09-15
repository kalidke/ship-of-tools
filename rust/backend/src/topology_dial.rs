// topology_dial.rs — the one-shot blocking client `sotd topology set` (and
// `status`'s "cache diverged" line) use to reach a daemon over its
// already-established endpoint spelling (`unix:`/`tcp:`/`pipe:`, per
// `topology::local_endpoint`/`relay_endpoint`). No new credential: per
// `op::TOPOLOGY_SET`'s own doc, the dial itself IS the authorisation, so
// this sends a plain unauthenticated `hello` (role `cli`) the same way any
// other one-shot shell caller would.
//
// Deliberately NOT the supervisor lane's `sot_log::client::Client` (pipe/
// socket challenge-auth trio): that machinery proves a CAPSULE's identity
// for the attach protocol, a different, heavier contract this plain
// control-socket hello has never needed.

use sot_protocol::{codec, Frame, HelloReq, Kind};

enum Conn {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(std::net::TcpStream),
    #[cfg(windows)]
    Pipe(std::fs::File),
}

impl Conn {
    fn try_clone(&self) -> std::io::Result<Conn> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.try_clone().map(Conn::Unix),
            Conn::Tcp(s) => s.try_clone().map(Conn::Tcp),
            #[cfg(windows)]
            Conn::Pipe(f) => f.try_clone().map(Conn::Pipe),
        }
    }
}

impl std::io::Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.read(buf),
            Conn::Tcp(s) => s.read(buf),
            #[cfg(windows)]
            Conn::Pipe(f) => f.read(buf),
        }
    }
}

impl std::io::Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.write(buf),
            Conn::Tcp(s) => s.write(buf),
            #[cfg(windows)]
            Conn::Pipe(f) => f.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.flush(),
            Conn::Tcp(s) => s.flush(),
            #[cfg(windows)]
            Conn::Pipe(f) => f.flush(),
        }
    }
}

fn connect(endpoint: &str) -> Result<Conn, String> {
    if let Some(p) = endpoint.strip_prefix("unix:") {
        #[cfg(unix)]
        {
            return std::os::unix::net::UnixStream::connect(p)
                .map(Conn::Unix)
                .map_err(|e| format!("{endpoint}: {e}"));
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            return Err(format!("{endpoint}: unix endpoints are POSIX-only"));
        }
    }
    if let Some(addr) = endpoint.strip_prefix("tcp:") {
        return std::net::TcpStream::connect(addr).map(Conn::Tcp).map_err(|e| format!("{endpoint}: {e}"));
    }
    if let Some(p) = endpoint.strip_prefix("pipe:") {
        #[cfg(windows)]
        {
            return std::fs::OpenOptions::new().read(true).write(true).open(p).map(Conn::Pipe).map_err(|e| format!("{endpoint}: {e}"));
        }
        #[cfg(not(windows))]
        {
            let _ = p;
            return Err(format!("{endpoint}: pipe endpoints are Windows-only"));
        }
    }
    Err(format!("{endpoint}: unrecognised endpoint spelling (expected unix:/tcp:/pipe:)"))
}

/// Dial `endpoint`, send a `cli`-role hello declaring `self_host`, then one
/// `req_op` request, and return its response payload verbatim (the caller
/// checks for `{"error": ..., "code": ...}` itself — same convention every
/// other daemon refusal already uses). A handful of unrelated evt frames
/// arriving before the matching `res` (unlikely on a connection this
/// short-lived, but the wire protocol allows it) are skipped rather than
/// treated as a protocol violation.
pub fn dial_and_call(endpoint: &str, self_host: &str, req_op: &str, payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let mut w = connect(endpoint)?;
    let r = w.try_clone().map_err(|e| format!("{endpoint}: {e}"))?;
    let mut br = std::io::BufReader::new(r);

    let hello = HelloReq {
        client_id: format!("sotd-topology-cli-{}", std::process::id()),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        host: Some(self_host.to_string()),
        role: "cli".to_string(),
        instance: None,
        name: Some(self_host.to_string()),
    };
    let hello_payload = serde_json::to_value(hello).map_err(|e| e.to_string())?;
    codec::write_frame_blocking(&mut w, &Frame::req(0, sot_protocol::op::HELLO, hello_payload))
        .map_err(|e| format!("{endpoint}: hello: {e}"))?;
    codec::read_frame_blocking(&mut br).map_err(|e| format!("{endpoint}: hello reply: {e}"))?;

    const REQ_ID: u64 = 1;
    codec::write_frame_blocking(&mut w, &Frame::req(REQ_ID, req_op, payload)).map_err(|e| format!("{endpoint}: {req_op}: {e}"))?;
    for _ in 0..8 {
        let frame = codec::read_frame_blocking(&mut br).map_err(|e| format!("{endpoint}: {req_op} reply: {e}"))?;
        if frame.kind == Kind::Res && frame.id == REQ_ID {
            return Ok(frame.payload);
        }
    }
    Err(format!("{endpoint}: no reply to {req_op} within 8 frames"))
}
