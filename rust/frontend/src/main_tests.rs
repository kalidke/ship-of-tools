use super::*;

/// ADR 0049, User isolation: the window's log never holds a page secret an event carries.
#[test]
fn the_window_log_masks_page_secrets() {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!("sot-window-log-{}-{nanos}.log", std::process::id()));
    let file = std::fs::File::create(&path).unwrap();
    let subscriber = log_subscriber(EnvFilter::new("trace"), move || file.try_clone().unwrap());
    let token = "0123456789abcdef0123456789abcdef";
    let _log = sot_log::test_log::install(subscriber);
    tracing::error!(%token, "open http://127.0.0.1:1/x?secret=Ab12Cd34");
    let written = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(written.contains("<redacted>"), "the event did not reach the file: {written}");
    assert!(!written.contains(token), "the token reached the file: {written}");
    assert!(!written.contains("Ab12Cd34"), "the secret reached the file: {written}");
    // No colour codes: on Windows this output is a file, and they would split a field name from its `=`.
    assert!(!written.contains('\u{1b}'), "the file carries ANSI escapes: {written:?}");
}

#[cfg(windows)]
fn stdio_flags() -> [u32; 3] {
    use windows_sys::Win32::Foundation::GetHandleInformation;
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|which| {
        let mut flags = 0;
        assert_ne!(
            unsafe { GetHandleInformation(GetStdHandle(which), &mut flags) },
            0
        );
        flags
    })
}

#[cfg(windows)]
fn owned_startup_probe(
    root: &std::path::Path,
) -> Result<(std::process::Child, sot_platform::test_isolated::Entry)> {
    use std::process::{Command, Stdio};
    let program = root.join("startup-probe.exe");
    sot_platform::test_exec::write_executable(&program, std::fs::read(std::env::current_exe()?)?);
    let (recipe, entry) =
        sot_platform::test_isolated::test_command("tests::window_startup_owned_probe");
    let mut command = Command::new(program);
    command.args(recipe.get_args());
    for (name, value) in recipe.get_envs() {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
    let probe = command
        .env("SOT_TEST_WINDOW_STARTUP_PROBE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    Ok((probe, entry))
}

#[cfg(windows)]
#[test]
fn window_startup_hardens_handles_before_starting_children() {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    if !sot_platform::test_isolated::run_isolated(
        "tests::window_startup_hardens_handles_before_starting_children",
    ) {
        return;
    }
    let _home = crate::net::state::test_env::set_test_env();
    let root = std::path::PathBuf::from(
        std::env::var_os("XDG_STATE_HOME").expect("fixture state root"),
    );
    let paths = ["input", "output", "error"].map(|name| root.join(name));
    let files = paths.each_ref().map(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
    });
    let which = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
    let saved = which.map(|handle| unsafe { GetStdHandle(handle) });
    struct Restore([windows_sys::Win32::Foundation::HANDLE; 3]);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (which, handle) in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
                .into_iter()
                .zip(self.0)
            {
                unsafe {
                    SetStdHandle(which, handle);
                }
            }
        }
    }
    let restore = Restore(saved);
    for (which, file) in which.into_iter().zip(&files) {
        assert_ne!(
            unsafe {
                SetHandleInformation(
                    file.as_raw_handle() as _,
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                )
            },
            0
        );
        assert_ne!(unsafe { SetStdHandle(which, file.as_raw_handle() as _) }, 0);
    }
    assert!(stdio_flags()
        .iter()
        .all(|flags| flags & HANDLE_FLAG_INHERIT != 0));
    let (flags, mut probe, entry) = window_entry(|| {
        let flags = stdio_flags();
        let (probe, entry) = owned_startup_probe(&root)?;
        Ok((flags, probe, entry))
    })
    .unwrap();
    drop(restore);
    drop(files);
    let exclusive = paths.each_ref().map(|path| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
    });
    probe.stdin.take();
    let pid = probe.id();
    let (status, stdout, stderr) =
        sot_platform::test_isolated::drain(probe).wait_within(std::time::Duration::from_secs(5));
    assert!(status.success(), "owned startup probe failed: {stderr}");
    entry.assert_once(pid);
    assert!(
        stdout.contains("startup-probe entered"),
        "the owned startup probe never entered"
    );
    assert!(
        flags.iter().all(|flags| flags & HANDLE_FLAG_INHERIT == 0),
        "window_entry left inherited standard handles before its startup continuation"
    );
    assert!(
        exclusive.iter().all(Result::is_ok),
        "the first owned child retained a fixture standard file"
    );
    println!("window-startup flags_cleared=3 child_retained_files=0 entered_bodies=1 completed_bodies=1");
}

#[cfg(windows)]
#[test]
fn window_startup_owned_probe() {
    if std::env::var("SOT_TEST_WINDOW_STARTUP_PROBE").as_deref() != Ok("1") {
        return;
    }
    sot_platform::test_isolated::enter("tests::window_startup_owned_probe");
    use std::io::{Read, Write};
    println!("startup-probe entered");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
}
