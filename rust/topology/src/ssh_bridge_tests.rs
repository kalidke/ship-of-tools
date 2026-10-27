use super::*;

#[test]
fn a_fixture_recipe_runs_its_fixture_command() {
    let plain = SshRecipe::new("hub", Some("far")).unwrap();
    let fixture = plain.clone().with_test_command("/fixture/prog", ["a", "b"]);
    let cmd = command(&fixture);
    assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/fixture/prog"), "fixture recipe ran ssh");
    assert_eq!(cmd.get_args().collect::<Vec<_>>(), [std::ffi::OsStr::new("a"), std::ffi::OsStr::new("b")]);
    let (program, args) = argv(&plain);
    let ssh = command(&plain);
    assert_eq!(ssh.get_program(), std::ffi::OsStr::new(program));
    assert_eq!(ssh.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(), args);
    assert_eq!(fixture.to_string(), "ssh:hub/far");
    assert_eq!(plain.to_string(), "ssh:hub/far");
}

#[cfg(unix)]
#[test]
fn a_child_keeps_no_descriptor_its_start_caught_before_close_on_exec() {
    use std::os::fd::AsRawFd;
    let (_reader, writer) = std::io::pipe().expect("pipe");
    let fd = writer.as_raw_fd();
    // SAFETY: fcntl changes only this process's flags on a descriptor the test holds open.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0, "clearing close-on-exec failed");
    let script = format!("if test -e /dev/fd/{fd}; then echo held; else echo none; fi");
    let recipe = SshRecipe::new("hub", None).unwrap().with_test_command("sh", ["-c", script.as_str()]);
    let ran = sot_platform::test_wait::run_within(&mut command(&recipe), std::time::Duration::from_secs(10)).ok();
    assert_eq!(ran.stdout.trim(), "none", "the child kept a descriptor its start caught before close-on-exec");
}

#[test]
fn recipe_rejects_a_leading_dash_in_either_half() {
    assert!(SshRecipe::new("-oProxyCommand=x", None).is_err());
    assert!(SshRecipe::new("hub", Some("-oProxyCommand=x")).is_err());
}

#[test]
fn recipe_rejects_grammar_outside_a_plain_host_name() {
    for bad in ["Hub", "hub;rm -rf", "hub name", "hub@host", ""] {
        assert!(SshRecipe::new(bad, None).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn argv_has_no_shell_and_the_stated_option_set() {
    let recipe = SshRecipe::new("hub", None).unwrap();
    let (program, args) = argv(&recipe);
    assert_eq!(program, "ssh");
    assert_eq!(
        args,
        vec![
            "-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ConnectTimeout=10", "-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ControlPersist=no", "hub",
            "export PATH=\"$HOME/.local/share/sot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH\"; sotd stdio-bridge",
        ]
    );
}

#[test]
fn argv_appends_host_only_when_the_recipe_carries_one() {
    let recipe = SshRecipe::new("hub", Some("gamma")).unwrap();
    let (_, args) = argv(&recipe);
    assert!(args.last().unwrap().ends_with("sotd stdio-bridge --host gamma"));

    let no_host = SshRecipe::new("hub", None).unwrap();
    let (_, args) = argv(&no_host);
    assert!(args.last().unwrap().ends_with("sotd stdio-bridge"));
    assert!(!args.last().unwrap().contains("--host"));
}

#[test]
fn display_matches_the_endpoint_grammar() {
    assert_eq!(SshRecipe::new("hub", None).unwrap().to_string(), "ssh:hub");
    assert_eq!(SshRecipe::new("hub", Some("gamma")).unwrap().to_string(), "ssh:hub/gamma");
}
