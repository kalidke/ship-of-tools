//! A local endpoint as an async stream: the window's control connection and lease, a relay socket the window dials,
//! and each connection the far router makes for a channel. The connect is `connect_own`, so a stream reaches only an
//! endpoint this OS account serves (ADR 0049, User isolation).

use std::path::Path;

/// The async stream a local endpoint is adopted into.
pub type LocalStream = interprocess::local_socket::tokio::Stream;

/// The client `connect_own` returns on this platform.
#[cfg(unix)]
pub type OwnClient = sot_platform::transport::socket_unix::SocketClient;
#[cfg(windows)]
pub type OwnClient = sot_platform::transport::pipe_win::PipeClient;

/// Connects through `connect_own` on a blocking thread, then adopts the socket or pipe. The fixed `CONNECT_BOUND`
/// retry budget lets an attempt or wait in progress finish (Unix's 20 ms retry sleep, the 200 ms named-pipe wait); it
/// is not an exact elapsed-time limit.
pub async fn connect_local(path: &Path) -> std::io::Result<LocalStream> {
    let owned = path.to_path_buf();
    let client = tokio::task::spawn_blocking(move || sot_platform::transport::connect_own::connect_own(&owned))
        .await
        .map_err(|e| std::io::Error::other(format!("the connect task: {e}")))?
        .map_err(sot_platform::transport::transport_error_to_io)?;
    adopt(client)
}

/// A connected client as an async stream, on the current runtime. Its cancel contract ends here.
#[cfg(unix)]
pub fn adopt(client: OwnClient) -> std::io::Result<LocalStream> {
    use interprocess::os::unix::uds_local_socket::tokio::Stream as UdsStream;
    let socket = client.into_stream();
    socket.set_nonblocking(true)?;
    let stream = UdsStream::try_from(std::os::fd::OwnedFd::from(socket)).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(LocalStream::from(stream))
}

/// A connected client as an async stream, on the current runtime: the handle `connect_own` opened for overlapped I/O,
/// as the lane bridge adopts its own (`rows/ops/lane_bridge.rs`).
#[cfg(windows)]
pub fn adopt(client: OwnClient) -> std::io::Result<LocalStream> {
    use interprocess::os::windows::named_pipe::local_socket::tokio::Stream as PipeStream;
    let stream = PipeStream::try_from(client.into_handle()).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(LocalStream::from(stream))
}
