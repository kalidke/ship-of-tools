use super::*;

#[test]
fn own_account_id_is_issued_by_the_os() {
    let id = own_account_id().expect("the OS issues this process's account");
    #[cfg(unix)]
    assert_eq!(id, format!("uid:{}", unsafe { libc::geteuid() }));
    #[cfg(windows)]
    assert!(id.starts_with("S-1-"), "{id}");
}
