use super::*;

#[test]
fn storage_codes_are_recognized_by_native_code_only() {
    #[cfg(unix)]
    let (full, other) = ([libc::ENOSPC, libc::EDQUOT], [libc::EIO, libc::EACCES]);
    #[cfg(windows)]
    let (full, other) = ([112, 39, 1295], [5, 1117]);
    for code in full {
        let io = || std::io::Error::from_raw_os_error(code);
        assert_eq!(storage_exhaustion(&Error::Io(io())), Some(code));
    }
    for code in other {
        let io = std::io::Error::from_raw_os_error(code);
        assert_eq!(storage_exhaustion(&Error::Io(io)), None);
    }
    let unsupported =
        std::io::Error::new(std::io::ErrorKind::Unsupported, "incompatible volume");
    assert_eq!(storage_exhaustion(&Error::Io(unsupported)), None);
    let text = Error::State("No space left on device (os error 28)".into());
    assert_eq!(storage_exhaustion(&text), None);
    let text_io = std::io::Error::new(
        std::io::ErrorKind::Other,
        "No space left on device (os error 28)",
    );
    assert_eq!(storage_exhaustion(&Error::Io(text_io)), None);
}
