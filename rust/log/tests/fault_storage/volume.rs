//! Owned native bounded volumes; provisioning failure is a failed premise, never a skip.
use super::*;
use std::io::Write;

pub fn on_volume(name: &str, body: impl FnOnce(&Path)) {
    #[cfg(target_os = "linux")]
    {
        if let Some(root) = std::env::var_os("L3_PREMISE_MOUNT") {
            sot_log::test_isolated::enter(name);
            body(Path::new(&root));
        } else {
            linux_child(name);
        }
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        let _ = name;
        let volume = NativeVolume::new();
        body(&volume.mount);
        volume.close();
    }
}

#[cfg(target_os = "linux")]
fn linux_child(name: &str) {
    let dir = tempfile::tempdir_in(scratch()).unwrap();
    let mount = dir.path().join("volume");
    std::fs::create_dir(&mount).unwrap();
    let (test, entry) = sot_log::test_isolated::test_command(name);
    let mut command = Command::new("unshare");
    command.args([
        "-Urm",
        "bash",
        "-c",
        r#"
set -eu
root=$1
shift
mount -t tmpfs -o size=8m tmpfs "$root"
set +e
"$@"
status=$?
set -e
umount "$root"
printf 'L3 fixture linux detach=ok\n'
exit "$status"
"#,
        "l3-premise",
    ]);
    command
        .arg(&mount)
        .arg(test.get_program())
        .args(test.get_args());
    for (key, value) in test.get_envs() {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    command.env("L3_PREMISE_MOUNT", &mount);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .expect("start the private user/mount namespace");
    let pid = child.id();
    let (status, out, err) =
        sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(120));
    let text = scrub(&format!("{out}{err}"));
    print!("{text}");
    // bash starts the selected test as its child, so the proof's PID is that test's,
    // not the namespace shell's. The helper still checks the complete exact record.
    let child_pid = text
        .lines()
        .find_map(|line| line.strip_prefix("L3 fixture-body pid="))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(pid);
    assert!(status.success(), "L3 native fixture child failed: {status}");
    entry.assert_once(child_pid);
    assert!(
        text.contains("L3 fixture linux detach=ok"),
        "Linux fixture teardown was not confirmed"
    );
}

#[cfg(any(target_os = "macos", windows))]
struct NativeVolume {
    dir: tempfile::TempDir,
    mount: PathBuf,
    attached: bool,
    cleanup_attempted: bool,
}

#[cfg(any(target_os = "macos", windows))]
impl NativeVolume {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(scratch()).unwrap();
        let mount = dir.path().join("volume");
        std::fs::create_dir(&mount).unwrap();
        let mut volume = Self {
            dir,
            mount,
            attached: false,
            cleanup_attempted: false,
        };
        volume.attach();
        // Capacity verification is before all allocation. A failed native attach
        // can therefore never turn this fixture into a fill of the host volume.
        assert!(
            capacity(&volume.mount) <= volume_limit(),
            "the mounted fixture exceeds its native bound"
        );
        volume
    }
    fn close(mut self) {
        self.detach();
    }
    #[cfg(target_os = "macos")]
    fn attach(&mut self) {
        let image = self.dir.path().join("volume.dmg");
        let mut create = Command::new("hdiutil");
        create
            .args([
                "create",
                "-size",
                "256m",
                "-fs",
                "APFS",
                "-type",
                "UDRW",
                "-layout",
                "GPTSPUD",
                "-volname",
                "L3Premise",
            ])
            .arg(&image);
        native_command(create);
        let mut attach = Command::new("hdiutil");
        attach
            .args(["attach", "-nobrowse", "-mountpoint"])
            .arg(&self.mount)
            .arg(&image);
        // Cleanup must also run if native attachment fails partway through.
        self.attached = true;
        native_command(attach);
    }
    #[cfg(target_os = "macos")]
    fn detach(&mut self) {
        if self.attached {
            self.cleanup_attempted = true;
            let mut command = Command::new("hdiutil");
            command.arg("detach").arg(&self.mount);
            native_command(command);
            self.attached = false;
            println!("L3 fixture macos detach=ok");
        }
    }
    #[cfg(windows)]
    fn diskpart(&self, name: &str, commands: &str) {
        let script = self.dir.path().join(name);
        std::fs::write(&script, commands).unwrap();
        let mut command = Command::new("diskpart.exe");
        command.arg("/s").arg(script);
        native_command(command);
    }
    #[cfg(windows)]
    fn attach(&mut self) {
        let image = self.dir.path().join("volume.vhd");
        let text = format!(
            "create vdisk file=\"{}\" maximum=64 type=fixed\nselect vdisk file=\"{}\"\nattach vdisk\ncreate partition primary\nformat fs=ntfs quick\nassign mount=\"{}\\\"\nexit\n",
            image.display(), image.display(), self.mount.display());
        // The selected image is exclusively ours, including partial setup.
        self.attached = true;
        self.diskpart("attach.txt", &text);
        use windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;
        let mount: Vec<u16> = format!("{}\\", self.mount.display())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut volume = [0u16; 128];
        let ok = unsafe {
            GetVolumeNameForVolumeMountPointW(
                mount.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            )
        };
        assert_ne!(
            ok,
            0,
            "VHD mount point was not attached: {}",
            std::io::Error::last_os_error()
        );
    }
    #[cfg(windows)]
    fn detach(&mut self) {
        if self.attached {
            self.cleanup_attempted = true;
            let image = self.dir.path().join("volume.vhd");
            self.diskpart(
                "detach.txt",
                &format!(
                    "select vdisk file=\"{}\"\ndetach vdisk\nexit\n",
                    image.display()
                ),
            );
            // An unattached VHD can be removed. A still-attached image is held open,
            // so a diskpart error that returned exit 0 is caught here as well.
            std::fs::remove_file(&image).expect("detach must release this fixture's backing image");
            self.attached = false;
            println!("L3 fixture windows detach=ok");
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
impl Drop for NativeVolume {
    fn drop(&mut self) {
        if self.attached && !self.cleanup_attempted {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.detach()));
            if result.is_err() {
                eprintln!("L3 fixture owned-volume cleanup FAILED");
            }
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
fn native_command(command: Command) {
    let (code, text) = bounded(command, 120);
    assert_eq!(code, 0, "native fixture setup/cleanup failed:\n{text}");
}

fn volume_limit() -> u64 {
    #[cfg(target_os = "linux")]
    {
        8 * 1024 * 1024
    }
    #[cfg(target_os = "macos")]
    {
        256 * 1024 * 1024
    }
    #[cfg(windows)]
    {
        64 * 1024 * 1024
    }
}

#[cfg(unix)]
fn capacity(root: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    assert_eq!(
        unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) },
        0
    );
    let stat = unsafe { stat.assume_init() };
    (stat.f_blocks as u64) * (stat.f_frsize as u64)
}

#[cfg(windows)]
fn capacity(root: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let path: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut total = 0;
    assert_ne!(
        unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                std::ptr::null_mut(),
                &mut total,
                std::ptr::null_mut(),
            )
        },
        0
    );
    total
}

pub fn fill(root: &Path) -> i32 {
    assert!(
        capacity(root) <= volume_limit(),
        "refusing to allocate on an unbounded filesystem"
    );
    println!("L3 fixture-body pid={}", std::process::id());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("ballast"))
        .unwrap();
    let mut block = [0u8; 65536];
    for (i, byte) in block.iter_mut().enumerate() {
        *byte = ((i * 73 + i / 251) % 256) as u8;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut allocated = 0;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "native fill exceeded its bound"
        );
        assert!(
            allocated <= volume_limit(),
            "native volume never reported exhaustion within its size bound"
        );
        match file.write(&block) {
            Ok(0) => {
                panic!("native volume returned a zero write instead of an OS exhaustion error")
            }
            Ok(n) => allocated += n as u64,
            Err(error) => {
                let code = error
                    .raw_os_error()
                    .expect("native fill must yield an OS code");
                assert!(
                    boundaries::codes().contains(&code),
                    "native fill failed for another reason: {error}"
                );
                return code;
            }
        }
    }
}

pub fn free_and_sync(root: &Path) {
    std::fs::remove_file(root.join("ballast")).expect("remove only the owned ballast");
    let path = root.join("after-free");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(b"l3-durable-after-free").unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::remove_file(path).unwrap();
    sot_log::host::preflight_volume(root).expect("preflight after freeing the native volume");
}
