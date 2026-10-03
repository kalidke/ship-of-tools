//! This process's OS account as the operating system issued it: the one value Ship of Tools compares when it must
//! tell two accounts on one box apart (decision 0031). Never a name read from the environment: two shells of one
//! account can disagree about `USER`/`USERNAME`, and an unset variable would switch a guard off without a word.

/// `uid:<effective uid>` on Unix; the process token's user SID (`S-1-5-…`) on Windows. `None` only when Windows
/// cannot read this process's own token.
pub fn own_account_id() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        Some(format!("uid:{}", unsafe { libc::geteuid() }))
    }
    #[cfg(windows)]
    {
        crate::fsutil::token_user_sid_string().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_account_id_is_issued_by_the_os() {
        let id = own_account_id().expect("the OS issues this process's account");
        #[cfg(unix)]
        assert_eq!(id, format!("uid:{}", unsafe { libc::geteuid() }));
        #[cfg(windows)]
        assert!(id.starts_with("S-1-"), "{id}");
    }
}
