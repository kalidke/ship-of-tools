//! Owned bounded volumes; setup, entry and teardown failures are harness failures.
use super::*;
use std::io::Write;

/// One volume test at a time: each fills its own volume.
static ONE_VOLUME: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn on_volume(body: impl FnOnce(&Path)) {
    let _one = ONE_VOLUME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    #[cfg(target_os = "linux")]
    {
        let root = std::env::var_os("L3_HOSTED_VOLUME_ROOT").unwrap_or_else(|| {
            panic!(
                "L3 HARNESS FAILURE: no bounded volume: on Linux the volume tests run only in rust.yml's L3 step"
            )
        });
        hosted_linux(Path::new(&root), body);
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        let volume = NativeVolume::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&volume.mount)));
        volume.close();
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }
}

#[cfg(target_os = "linux")]
fn hosted_linux(root: &Path, body: impl FnOnce(&Path)) {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    assert!(
        root.is_absolute(),
        "L3 HARNESS FAILURE: hosted root is relative"
    );
    let root = root.canonicalize().expect("existing hosted mount");
    let declared: u64 = std::env::var("L3_HOSTED_VOLUME_BYTES")
        .expect("declared hosted capacity")
        .parse()
        .expect("numeric hosted capacity");
    assert_eq!(
        declared,
        64 * 1024 * 1024,
        "L3 HARNESS FAILURE: unexpected hosted size"
    );
    let path = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    assert_eq!(unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) }, 0);
    assert_eq!(
        unsafe { stat.assume_init() }.f_type,
        libc::EXT4_SUPER_MAGIC,
        "L3 HARNESS FAILURE: hosted volume must be ext4"
    );
    let device = std::fs::metadata(&root).unwrap().dev();
    assert_ne!(
        device,
        std::fs::metadata(scratch()).unwrap().dev(),
        "hosted fixture shares scratch filesystem"
    );
    assert_ne!(
        device,
        std::fs::metadata(root.parent().unwrap()).unwrap().dev(),
        "hosted root is not a mount"
    );
    let bytes = capacity(&root);
    assert!(
        bytes > 0 && bytes <= declared,
        "L3 HARNESS FAILURE: hosted volume exceeds declared capacity"
    );
    println!("L3 fixture linux mode=hosted-ext4 declared-bytes={declared} capacity={bytes} device={device} mount=validated");
    let subtree = tempfile::Builder::new()
        .prefix("l3-body-")
        .tempdir_in(root)
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(subtree.path())));
    subtree
        .close()
        .expect("L3 HARNESS FAILURE: owned hosted subtree cleanup failed");
    println!("L3 fixture linux subtree-cleanup=ok");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[cfg(any(target_os = "macos", windows))]
struct NativeVolume {
    dir: Option<tempfile::TempDir>,
    mount: PathBuf,
    image: PathBuf,
    #[cfg(windows)]
    image_native: String,
    #[cfg(windows)]
    mount_native: String,
    #[cfg(windows)]
    volume_identity: Option<String>,
    #[cfg(windows)]
    transaction: uuid::Uuid,
    attached: bool,
    cleanup_attempted: bool,
}

#[cfg(any(target_os = "macos", windows))]
impl NativeVolume {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("l3 premise ")
            .tempdir_in(scratch())
            .unwrap();
        let mount = dir.path().join("volume");
        std::fs::create_dir(&mount).unwrap();
        #[cfg(target_os = "macos")]
        let image = dir.path().join("volume.dmg");
        #[cfg(windows)]
        let image = dir.path().join("volume.vhd");
        #[cfg(windows)]
        let native = native_path(&dir.path().canonicalize().unwrap());
        let mut volume = Self {
            #[cfg(windows)]
            image_native: format!("{native}\\volume.vhd"),
            #[cfg(windows)]
            mount_native: format!("{native}\\volume\\"),
            #[cfg(windows)]
            volume_identity: None,
            #[cfg(windows)]
            transaction: uuid::Uuid::now_v7(),
            dir: Some(dir),
            mount,
            image,
            attached: false,
            cleanup_attempted: false,
        };
        volume.attach();
        let bytes = capacity(&volume.mount);
        assert!(
            bytes > 0 && bytes <= volume_limit(),
            "L3 HARNESS FAILURE: mounted fixture exceeds native bound"
        );
        println!("L3 fixture native mount=validated capacity={bytes}");
        volume
    }

    fn close(mut self) {
        self.cleanup();
    }

    fn cleanup(&mut self) {
        self.detach();
        if let Some(dir) = self.dir.take() {
            dir.close()
                .expect("L3 HARNESS FAILURE: owned image directory cleanup failed");
        }
    }

    #[cfg(target_os = "macos")]
    fn attach(&mut self) {
        let mut create = Command::new("hdiutil");
        create
            .args([
                "create",
                "-size",
                "256m",
                "-fs",
                "APFS",
                "-type",
                "UDIF",
                "-layout",
                "GPTSPUD",
                "-volname",
                "L3Premise",
            ])
            .arg(&self.image);
        native_command(create);
        let mut info = Command::new("hdiutil");
        info.args(["imageinfo", "-format"]).arg(&self.image);
        let format = native_command(info);
        assert_eq!(format.trim(), "UDRW", "L3 HARNESS FAILURE: image format");
        println!("L3 fixture macos image-type=UDIF format=UDRW declared-bytes=268435456");
        let mut attach = Command::new("hdiutil");
        attach
            .args(["attach", "-nobrowse", "-mountpoint"])
            .arg(&self.mount)
            .arg(&self.image);
        self.attached = true;
        native_command(attach);
        use std::os::unix::fs::MetadataExt;
        assert_ne!(
            std::fs::metadata(&self.mount).unwrap().dev(),
            std::fs::metadata(self.dir.as_ref().unwrap().path())
                .unwrap()
                .dev(),
            "L3 HARNESS FAILURE: image not mounted"
        );
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(self.mount.as_os_str().as_bytes()).unwrap();
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        assert_eq!(unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) }, 0);
        let stat = unsafe { stat.assume_init() };
        let filesystem = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) };
        assert_eq!(
            filesystem.to_bytes(),
            b"apfs",
            "L3 HARNESS FAILURE: mounted filesystem"
        );
        println!("L3 fixture macos attach=ok filesystem=apfs mount=validated");
    }

    #[cfg(target_os = "macos")]
    fn detach(&mut self) {
        if self.attached {
            self.cleanup_attempted = true;
            let mut command = Command::new("hdiutil");
            command.arg("detach").arg(&self.mount);
            native_command(command);
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&self.mount).unwrap().dev(),
                std::fs::metadata(self.dir.as_ref().unwrap().path())
                    .unwrap()
                    .dev(),
                "L3 HARNESS FAILURE: image still mounted"
            );
            self.attached = false;
            println!("L3 fixture macos detach=ok");
        }
    }

    #[cfg(windows)]
    fn diskpart(&self, name: &str, commands: &str) {
        let script = self.dir.as_ref().unwrap().path().join(name);
        std::fs::write(&script, commands).unwrap();
        let mut command = Command::new("diskpart.exe");
        command
            .arg("/s")
            .arg(native_path(&script.canonicalize().unwrap()));
        let (code, output) = bounded(command, 120);
        let text = format!("{commands}\n{output}")
            .replace(&self.image_native, "<owned-image>")
            .replace(&self.mount_native, "<owned-mount>");
        let text = text
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("On computer:") {
                    "On computer: <host>"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = scrub(&text);
        let logs = scratch().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let log = logs.join(format!("l3-diskpart-{}-{name}.log", self.transaction));
        let transcript = format!(
            "L3 diskpart transaction={} script={name} exit={code}\n{text}\n",
            self.transaction
        );
        std::fs::write(log, &transcript).expect("retain the sanitized DiskPart transcript");
        print!("{transcript}");
        assert_eq!(code, 0, "L3 HARNESS FAILURE: DiskPart transaction failed");
    }

    #[cfg(windows)]
    fn attach(&mut self) {
        let text = format!(
            "create vdisk file=\"{}\" maximum=64 type=fixed\nselect vdisk file=\"{}\"\nattach vdisk\ncreate partition primary\nformat fs=ntfs quick\nassign mount=\"{}\"\ndetail vdisk\nexit\n",
            self.image_native, self.image_native, self.mount_native);
        self.attached = true;
        self.diskpart("attach.txt", &text);
        let identity = self.mount_identity();
        println!("L3 fixture windows transaction={} image=<owned-image> mount=<owned-mount> volume={identity}", self.transaction);
        self.volume_identity = Some(identity);
        println!("L3 fixture windows native-path=validated space-directory=yes attach=ok");
    }

    #[cfg(windows)]
    fn mount_identity(&self) -> String {
        use windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;
        let mount: Vec<u16> = self.mount_native.encode_utf16().chain(Some(0)).collect();
        let mut volume = [0u16; 128];
        let result = unsafe {
            GetVolumeNameForVolumeMountPointW(
                mount.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            )
        };
        assert_ne!(
            result,
            0,
            "L3 HARNESS FAILURE: mount identity: {}",
            std::io::Error::last_os_error()
        );
        let length = volume
            .iter()
            .position(|&c| c == 0)
            .expect("terminated volume identity");
        String::from_utf16(&volume[..length]).expect("Unicode volume identity")
    }

    #[cfg(windows)]
    fn mount_attributes(&self) -> u32 {
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
        };
        let mount: Vec<u16> = self
            .mount_native
            .trim_end_matches('\\')
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let attributes = unsafe { GetFileAttributesW(mount.as_ptr()) };
        assert_ne!(
            attributes,
            INVALID_FILE_ATTRIBUTES,
            "L3 HARNESS FAILURE: inspect owned mount: {}",
            std::io::Error::last_os_error()
        );
        attributes
    }

    #[cfg(windows)]
    fn remove_owned_mount(&self) {
        use windows_sys::Win32::Storage::FileSystem::{
            DeleteVolumeMountPointW, FILE_ATTRIBUTE_REPARSE_POINT,
        };
        if self.mount_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            assert_eq!(
                Some(self.mount_identity()).as_ref(),
                self.volume_identity.as_ref(),
                "L3 HARNESS FAILURE: owned mount identity changed"
            );
            let mount: Vec<u16> = self.mount_native.encode_utf16().chain(Some(0)).collect();
            assert_ne!(
                unsafe { DeleteVolumeMountPointW(mount.as_ptr()) },
                0,
                "L3 HARNESS FAILURE: remove owned mount: {}",
                std::io::Error::last_os_error()
            );
        }
        assert_eq!(
            self.mount_attributes() & FILE_ATTRIBUTE_REPARSE_POINT,
            0,
            "L3 HARNESS FAILURE: owned mount association remains"
        );
        println!(
            "L3 fixture windows transaction={} mount-release=ok identity=checked",
            self.transaction
        );
    }

    #[cfg(windows)]
    fn detach(&mut self) {
        if self.attached {
            self.cleanup_attempted = true;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if self.mount_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                let identity = self.mount_identity();
                if let Some(retained) = &self.volume_identity {
                    assert_eq!(
                        &identity, retained,
                        "L3 HARNESS FAILURE: attached mount identity changed"
                    );
                } else {
                    self.volume_identity = Some(identity);
                }
            }
            self.diskpart(
                "detach.txt",
                &format!(
                    "select vdisk file=\"{}\"\ndetail vdisk\ndetach vdisk\ndetail vdisk\nexit\n",
                    self.image_native
                ),
            );
            self.remove_owned_mount();
            std::fs::remove_file(&self.image)
                .expect("L3 HARNESS FAILURE: detach must release the owned backing image");
            self.attached = false;
            println!("L3 fixture windows detach=ok mount-release=ok image-removal=ok");
        }
    }
}

#[cfg(windows)]
fn native_path(path: &Path) -> String {
    use std::path::{Component, Prefix};
    assert!(
        path.is_absolute(),
        "L3 HARNESS FAILURE: native path is not absolute"
    );
    let mut components = path.components();
    let prefix = components.next().expect("native prefix");
    let text = path
        .to_str()
        .expect("L3 HARNESS FAILURE: native path is not Unicode");
    let text = match prefix {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(_) => text,
            Prefix::VerbatimDisk(_) => text.strip_prefix(r"\\?\").expect("verbatim disk prefix"),
            _ => panic!("L3 HARNESS FAILURE: native path must be a local drive path"),
        },
        _ => panic!("L3 HARNESS FAILURE: native path must be drive qualified"),
    };
    let mut normalized = Path::new(text).components();
    assert!(
        matches!(normalized.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)))
            && matches!(normalized.next(), Some(Component::RootDir)),
        "L3 HARNESS FAILURE: invalid native drive root"
    );
    assert!(
        !text.contains(['"', '\r', '\n']),
        "L3 HARNESS FAILURE: native path contains script delimiters"
    );
    text.trim_end_matches('\\').to_string()
}

#[cfg(windows)]
pub fn windows_unwind_control() {
    let volume = NativeVolume::new();
    let dir = volume.dir.as_ref().unwrap().path().to_path_buf();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _volume = volume;
        panic!("L3 harness injected failure after attach");
    }));
    assert!(
        result.is_err() && !dir.exists(),
        "L3 HARNESS FAILURE: partial attach unwind retained resources"
    );
    println!("L3 harness windows after-attach-unwind=ok image-removal=ok");
}

#[cfg(any(target_os = "macos", windows))]
impl Drop for NativeVolume {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.cleanup()));
            if result.is_err() {
                eprintln!("L3 HARNESS FAILURE: owned-volume cleanup FAILED");
            }
        }
        if self.attached {
            if let Some(dir) = self.dir.take() {
                let _retained = dir.keep();
            }
            eprintln!("L3 HARNESS FAILURE: attached image retained for fixture owner");
        }
    }
}

#[cfg(target_os = "macos")]
fn native_command(command: Command) -> String {
    let (code, text) = bounded(command, 120);
    assert_eq!(
        code,
        0,
        "L3 HARNESS FAILURE: native fixture setup/cleanup failed:\n{}",
        scrub(&text)
    );
    print!("{}", scrub(&text));
    text
}

fn volume_limit() -> u64 {
    #[cfg(target_os = "linux")]
    {
        64 * 1024 * 1024
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

/// Writes the ballast until a one-byte write fails, then makes folders beside it at `root` until one cannot be made;
/// returns the native code of the data write that found the volume full.
pub fn fill(root: &Path) -> i32 {
    assert!(
        capacity(root) <= volume_limit(),
        "refusing to allocate on an unbounded filesystem"
    );
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
    // A write that does not fit may fail whole (NTFS) instead of short (ext4): after each exhaustion error the next
    // write is half as long, and the volume is full only when a one-byte write fails.
    let mut size = block.len();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "native fill exceeded its bound at write size {size} after {allocated} bytes"
        );
        assert!(
            allocated <= volume_limit(),
            "native volume never reported exhaustion within its size bound"
        );
        match file.write(&block[..size]) {
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
                if size == 1 {
                    fill_folders(root, deadline);
                    return code;
                }
                size /= 2;
            }
        }
    }
}

/// A volume with no data space left can still take a folder where the filesystem keeps a small folder in its own
/// table (NTFS's master file table): folders are made beside the ballast until one cannot be.
fn fill_folders(root: &Path, deadline: std::time::Instant) {
    let mut made = 0u32;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "native fill exceeded its bound after {made} folders"
        );
        match std::fs::create_dir(root.join(format!("ballast-{made}"))) {
            Ok(()) => made += 1,
            Err(error) => {
                let code = error
                    .raw_os_error()
                    .expect("native fill must yield an OS code");
                assert!(
                    boundaries::codes().contains(&code),
                    "native fill's folder failed for another reason: {error}"
                );
                println!("L3 fill folders={made} folder-code={code}");
                return;
            }
        }
    }
}

pub fn free_and_sync(root: &Path) {
    std::fs::remove_file(root.join("ballast")).expect("remove only the owned ballast");
    for entry in std::fs::read_dir(root).expect("list the owned volume") {
        let entry = entry.expect("an owned volume entry");
        if entry.file_name().to_string_lossy().starts_with("ballast-") {
            std::fs::remove_dir(entry.path()).expect("remove only the owned ballast folders");
        }
    }
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
