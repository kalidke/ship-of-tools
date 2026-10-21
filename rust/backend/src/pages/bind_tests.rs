/// ADR 0049, User isolation: a page listener is not inherited by the daemon's child processes.
#[tokio::test]
async fn a_page_listener_is_not_inheritable() {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
    let listener = super::bind_page_listener(0).unwrap();
    let mut flags = 0u32;
    let ok = unsafe { GetHandleInformation(listener.as_raw_socket() as _, &mut flags) };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0, "the page listener's socket is inheritable");
}
