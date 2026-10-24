use super::*;

#[test]
fn every_error_kind_has_its_action() {
    let kind = |k: io::ErrorKind| accept_policy(&io::Error::from(k));
    assert_eq!(kind(io::ErrorKind::Interrupted), AcceptAction::Retry);
    assert_eq!(kind(io::ErrorKind::ConnectionAborted), AcceptAction::Retry);
    assert_eq!(kind(io::ErrorKind::WouldBlock), AcceptAction::Retry);
    assert_eq!(kind(io::ErrorKind::OutOfMemory), AcceptAction::Sleep);
    assert_eq!(kind(io::ErrorKind::PermissionDenied), AcceptAction::Dead);
}

#[cfg(unix)]
#[test]
fn resource_errors_sleep_and_a_bad_listener_is_dead() {
    let os = |c: i32| accept_policy(&io::Error::from_raw_os_error(c));
    for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
        assert_eq!(os(code), AcceptAction::Sleep, "errno {code}");
    }
    assert_eq!(os(libc::ECONNABORTED), AcceptAction::Retry);
    assert_eq!(os(libc::EBADF), AcceptAction::Dead);
    assert_eq!(os(libc::EINVAL), AcceptAction::Dead);
}
