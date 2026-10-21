use super::session_socket_path;
use std::path::PathBuf;

#[test]
fn session_socket_path_is_a_named_pipe_per_user_and_label() {
    // SAFETY: no other test in this crate mutates USERNAME.
    struct Guard(Option<std::ffi::OsString>);
    impl Drop for Guard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("USERNAME", v),
                None => std::env::remove_var("USERNAME"),
            }
        }
    }
    let _g = Guard(std::env::var_os("USERNAME"));
    std::env::set_var("USERNAME", "TestUser");
    assert_eq!(
        session_socket_path("local"),
        PathBuf::from(r"\\.\pipe\sot-TestUser-local")
    );
    // The label goes through the same `slug()` every platform uses: a
    // `.` (illegal in a tmux session name, and a source of round-trip
    // mismatches elsewhere) is substituted here too, not just on Unix.
    assert_eq!(
        session_socket_path("MyPackage.jl"),
        PathBuf::from(r"\\.\pipe\sot-TestUser-mypackage_jl")
    );
}
