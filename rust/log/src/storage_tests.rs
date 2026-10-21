use super::*;
use crate::lane::transport::TransportError;

#[test]
fn a_transport_error_is_never_storage_exhaustion() {
    #[cfg(unix)]
    let full = [libc::ENOSPC, libc::EDQUOT];
    #[cfg(windows)]
    let full = [112, 39, 1295];
    for code in full {
        let io = || std::io::Error::from_raw_os_error(code);
        assert_eq!(storage_exhaustion(&Error::Io(io())), Some(code));
        let transport = TransportError::Io { op: "write", source: io() };
        assert_eq!(storage_exhaustion(&Error::Transport(transport)), None);
        let runtime = TransportError::RuntimeDir(io());
        assert_eq!(storage_exhaustion(&Error::Transport(runtime)), None);
    }
    let text = Error::State("No space left on device (os error 28)".into());
    assert_eq!(storage_exhaustion(&text), None);
}
