//! Snapshot preparation and executed entry controls; no product recovery owner.
use super::*;
use sha2::Digest;

pub fn boundary_gate() {
    let _cases = [
        boundaries::spawn_and_bind_preserve_native_storage_error as fn(),
        boundaries::nonstorage_errors_keep_severity,
        boundaries::real_full_volume_before_ready,
        boundaries::real_full_volume_after_output_starts,
        boundaries::harness_real_body,
        boundaries::harness_assertion_failure,
        boundaries::harness_duplicate_entry,
    ];
    let dir = tempfile::tempdir_in(scratch()).unwrap();
    let mut prepare = Command::new("python3");
    prepare
        .arg("-c")
        .arg(PREPARE)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")))
        .arg(dir.path());
    let (code, text) = bounded(prepare, 120);
    assert_eq!(
        code,
        0,
        "premise snapshot preparation failed:\n{}",
        scrub(&text)
    );
    print!("{}", scrub(&text));
    let mut snapshots = Vec::new();
    for label in ["parent", "head", "revert"] {
        let artifacts = [
            build_artifact(dir.path(), label, "lib"),
            build_artifact(dir.path(), label, "integration"),
        ];
        prove_listing(&artifacts);
        harness_controls(&artifacts[1]);
        snapshots.push(artifacts);
    }
    isolation_controls(&snapshots[1][0]);
    let mut executed = 0;
    for artifacts in &snapshots {
        // Actual nonstorage and output controls precede accepting any assertion red.
        for (target, name, red) in selections()
            .into_iter()
            .filter(|case| !case.2)
            .chain(selections().into_iter().filter(|case| case.2))
        {
            let artifact = &artifacts[usize::from(target != "lib")];
            let expected = if artifact.label == "head" || !red {
                0
            } else {
                101
            };
            let (code, text) = selected(artifact, name, false);
            assert!(
                text.contains("running 1 test") && text.contains(name),
                "premise selection ran no body"
            );
            assert_eq!(
                code, expected,
                "premise did not produce its intended native red/green result"
            );
            if expected == 101 {
                assert!(
                    text.contains("L3 P1")
                        && (text.contains("native code was erased")
                            || text.contains("original Error::Io/raw_os_error")
                            || text.contains("must not become sealed SpawnFailed")
                            || text.contains("erased the native full-volume code")),
                    "premise failed for another reason"
                );
                assert!(
                    !text.contains("L3 HARNESS FAILURE"),
                    "fixture failure cannot supply an assertion red"
                );
            }
            println!(
                "P1-{}-{name} exit {code} expected {expected}",
                artifact.label
            );
            executed += 1;
        }
    }
    assert_eq!(executed, selections().len() * snapshots.len());
    println!(
        "L3 P1 selections-per-snapshot={} snapshots={} runs={executed}",
        selections().len(),
        snapshots.len()
    );
    println!("L3 P1 parent=red prospective=green reversal=red controls=green");
}

struct Artifact {
    label: String,
    root: PathBuf,
    executable: PathBuf,
    identity: String,
}

fn build_artifact(root: &Path, label: &str, target: &str) -> Artifact {
    let root = root.join(label).join("rust");
    let manifest = std::fs::read(root.parent().unwrap().join("source-hashes.json")).unwrap();
    let digest = format!("{:x}", sha2::Sha256::digest(manifest));
    let identity = format!("l3-premise-{label}-{}", &digest[..40]);
    let mut command = Command::new("nice");
    command
        .arg("cargo")
        .current_dir(&root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_PROFILE_DEV_DEBUG", "line-tables-only")
        .env("SOT_BUILD_ID", &identity);
    owned_umask(&mut command);
    command
        .args([
            "test",
            "-p",
            "sot-log",
            "--locked",
            "-j",
            "8",
            "--no-run",
            "--message-format=json",
        ])
        .arg("--target-dir")
        .arg(scratch().join("l3-premise-build").join(label));
    let (target_name, source) = if target == "lib" {
        command.arg("--lib");
        ("sot_log", "log/src/lib.rs")
    } else {
        command.args(["--test", "fault_storage"]);
        ("fault_storage", "log/tests/fault_storage/main.rs")
    };
    let (code, text) = bounded(command, 600);
    save_log(&format!("build-{label}-{target}"), &text);
    assert_eq!(
        code,
        0,
        "L3 HARNESS FAILURE: artifact build failed:\n{}",
        scrub(&text)
    );
    let expected_source = root.join(source).canonicalize().unwrap();
    let mut matches = Vec::new();
    for line in text.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact"
            || message["profile"]["test"] != true
            || message["target"]["name"] != target_name
        {
            continue;
        }
        let package = message["package_id"].as_str().expect("artifact package id");
        let source = Path::new(
            message["target"]["src_path"]
                .as_str()
                .expect("artifact source"),
        );
        if !package
            .split('#')
            .next_back()
            .unwrap()
            .starts_with("sot-log@")
            || source.canonicalize().unwrap() != expected_source
        {
            continue;
        }
        if let Some(path) = message["executable"].as_str() {
            matches.push(PathBuf::from(path));
        }
    }
    assert_eq!(
        matches.len(),
        1,
        "L3 HARNESS FAILURE: absent or ambiguous {target_name} artifact"
    );
    let executable = matches.pop().unwrap();
    assert!(
        executable.is_absolute() && executable.is_file(),
        "artifact must be an absolute existing executable"
    );
    println!(
        "{}",
        scrub(&format!(
            "L3 artifact snapshot={label} target={target_name} build-id={identity} executable={}\n",
            executable.display()
        ))
    );
    Artifact {
        label: label.into(),
        root,
        executable,
        identity,
    }
}

fn owned_umask(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                libc::umask(0o022);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    let _ = command;
}

fn prove_listing(artifacts: &[Artifact; 2]) {
    for (index, artifact) in artifacts.iter().enumerate() {
        let mut command = Command::new(&artifact.executable);
        command.arg("--list").current_dir(&artifact.root);
        let (code, text) = bounded(command, 120);
        assert_eq!(code, 0, "L3 HARNESS FAILURE: runtime listing failed");
        for (_, name, _) in selections()
            .into_iter()
            .filter(|case| usize::from(case.0 != "lib") == index)
        {
            let count = text
                .lines()
                .filter(|line| *line == format!("{name}: test"))
                .count();
            assert_eq!(
                count, 1,
                "L3 HARNESS FAILURE: missing or ambiguous listed {name}"
            );
            println!(
                "L3 listing snapshot={} name={name} members={count}",
                artifact.label
            );
        }
    }
}

fn selected(artifact: &Artifact, name: &str, reject_entry: bool) -> (i32, String) {
    let (selection, entry) = sot_log::test_isolated::test_command(name);
    let mut command = Command::new(&artifact.executable);
    command
        .args(selection.get_args())
        .current_dir(&artifact.root);
    for (key, value) in selection.get_envs() {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    command.env("CARGO_TARGET_DIR", scratch().parent().unwrap());
    #[cfg(windows)]
    if artifact.label != "head" && name == "boundaries::real_full_volume_before_ready" {
        command.env("L3_WINDOWS_PARENT_OBSERVATION", "1");
    }
    owned_umask(&mut command);
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the selected snapshot executable");
    let pid = child.id();
    let (status, out, err) =
        sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(600));
    let text = format!("{out}{err}");
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(pid)));
    save_log(
        &format!("{}-{}", artifact.label, name.replace("::", "-")),
        &text,
    );
    print!("{}", scrub(&text));
    if reject_entry {
        let payload = checked.expect_err("L3 HARNESS FAILURE: invalid entry was accepted");
        let message = panic_message(&payload);
        let expected = if name.ends_with("harness_duplicate_entry") {
            "other than once"
        } else {
            "did not enter"
        };
        assert!(
            message.contains(expected),
            "unexpected entry rejection: {message}"
        );
    } else if let Err(payload) = checked {
        std::panic::resume_unwind(payload);
    }
    println!(
        "L3 invocation snapshot={} build-id={} name={name} pid={pid} entry={}",
        artifact.label,
        artifact.identity,
        if reject_entry { "rejected" } else { "once" }
    );
    (status.code().unwrap_or(-1), text)
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-text panic")
}

fn harness_controls(artifact: &Artifact) {
    for (name, code, reject) in [
        ("boundaries::harness_real_body", 0, false),
        ("boundaries::harness_wrong_qualified_name", 0, true),
        ("boundaries::harness_assertion_failure", 101, false),
        ("boundaries::harness_duplicate_entry", 0, true),
    ] {
        let (actual, text) = selected(artifact, name, reject);
        assert_eq!(actual, code, "L3 HARNESS FAILURE: control exit differs");
        if code == 101 {
            assert!(text.contains("L3 harness intentional assertion failure"));
        }
        println!(
            "L3 harness-control snapshot={} name={name} exit {actual} expected {code}",
            artifact.label
        );
    }
    println!("L3 harness snapshot={} qualified=accepted zero-body=rejected expected-failure=accepted duplicate-entry=rejected", artifact.label);
}

fn isolation_controls(artifact: &Artifact) {
    for name in [
        "a_real_body_enters_exactly_once",
        "an_entry_dropped_unchecked_fails",
        "a_child_that_fills_its_pipe_is_drained_and_finishes",
        "a_stalled_child_fails_at_its_bound",
        "output_a_descendant_holds_fails_at_the_bound",
    ] {
        let full = format!("test_isolated::tests::{name}");
        let (code, text) = selected(artifact, &full, false);
        assert_eq!(
            code,
            0,
            "L3 HARNESS FAILURE: inherited isolation control:\n{}",
            scrub(&text)
        );
        assert!(
            text.contains("1 passed; 0 failed"),
            "inherited control ran no body"
        );
        println!("L3 isolation-control {name} result=ok count=1");
    }
}

fn save_log(name: &str, text: &str) {
    let logs = scratch().parent().unwrap().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join(format!("l3-P1-{name}.log")), scrub(text)).unwrap();
}

fn selections() -> Vec<(&'static str, &'static str, bool)> {
    let mut cases = vec![
        (
            "integration",
            "boundaries::spawn_and_bind_preserve_native_storage_error",
            true,
        ),
        (
            "integration",
            "boundaries::nonstorage_errors_keep_severity",
            false,
        ),
        (
            "integration",
            "boundaries::real_full_volume_after_output_starts",
            false,
        ),
    ];
    #[cfg(unix)]
    cases.extend([
        (
            "integration",
            "boundaries::real_full_volume_before_ready",
            true,
        ),
        (
            "lib",
            "host::volume::l3_premises::preflight_preserves_native_storage_error_create",
            true,
        ),
        (
            "lib",
            "host::volume::l3_premises::preflight_preserves_native_storage_error_rename",
            true,
        ),
        (
            "lib",
            "host::volume::l3_premises::preflight_preserves_native_storage_error_recreate",
            true,
        ),
        (
            "lib",
            "host::volume::l3_premises::preflight_preserves_native_storage_error_fsync",
            true,
        ),
        (
            "lib",
            "host::volume::l3_premises::nonstorage_errors_keep_severity",
            false,
        ),
    ]);
    #[cfg(windows)]
    cases.extend([
        (
            "integration",
            "boundaries::real_full_volume_before_ready",
            false,
        ),
        (
            "lib",
            "host::l3_premises::windows_context_preserves_full_codes_disk",
            true,
        ),
        (
            "lib",
            "host::l3_premises::windows_context_preserves_full_codes_handle",
            true,
        ),
    ]);
    cases.extend([
        ("lib", "supervisor::journal::reset::l3_premises::reset_preserves_native_storage_error_flush", true),
        ("lib", "supervisor::journal::reset::l3_premises::reset_preserves_native_storage_error_rename", true),
        ("lib", "supervisor::journal::reset::l3_premises::reset_preserves_native_storage_error_create", true),
        ("lib", "supervisor::journal::reset::l3_premises::reset_preserves_native_storage_error_bootstrap", true),
        ("lib", "supervisor::journal::reset::l3_premises::reset_preserves_native_storage_error_pointer", true),
        ("lib", "supervisor::journal::reset::l3_premises::nonstorage_errors_keep_severity", false),
    ]);
    cases
}

pub const PREPARE: &str = r###""""Build immutable premise source copies; called by the temporary CI suite as well."""
from pathlib import Path
import hashlib, io, json, subprocess, sys, zipfile
BASE = '8c5f0d24f733f563cc8b30d292fe37f8d2ab640d'
repo, destination = map(Path, sys.argv[1:3])
git_repo=Path(subprocess.check_output(['git','rev-parse','--show-toplevel'],cwd=repo,text=True).strip())
HEAD = subprocess.check_output(['git','rev-parse','HEAD'],cwd=git_repo,text=True).strip()
print('L3 preparation checkout=validated premise-head='+HEAD+' base='+BASE)
product = subprocess.check_output(['git','diff',BASE,'HEAD','--','rust/log/src'],cwd=git_repo)
assert not product, 'premise branch must contain no product edits'
archive = subprocess.check_output(['git','archive','--format=zip','HEAD','rust'],cwd=git_repo)
destination.mkdir(parents=True,exist_ok=True)

def replace(text, old, new, count=1):
    assert text.count(old) == count, 'premise source anchor changed: '+old[:60]
    return text.replace(old,new)

MACRO = r'''
// Passive test-only IO overlay. It never classifies, retries or changes ownership.
#[cfg(test)]
thread_local! { static L3_FAULT: std::cell::RefCell<Option<(&'static str, i32)>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn l3_take_fault(point: &str) -> Option<i32> {
    L3_FAULT.with(|fault| {
        let mut fault = fault.borrow_mut();
        if fault.as_ref().is_some_and(|(at, _)| *at == point) { fault.take().map(|(_, code)| code) } else { None }
    })
}
#[cfg(test)]
macro_rules! l3_fault {
    ($point:literal, $operation:expr) => {
        if let Some(code) = l3_take_fault($point) { Err(std::io::Error::from_raw_os_error(code).into()) } else { $operation }
    };
}
#[cfg(not(test))]
macro_rules! l3_fault { ($point:literal, $operation:expr) => { $operation }; }
'''

def volume_overlay(text):
    text=replace(text,'use crate::{Error, Result};','use crate::{Error, Result};\n'+MACRO)
    for point,expression in [('fsync','fsync_dir(dir)'),('create','kind.create(&a, first)'),('recreate','kind.create(&a, second)')]:
        text=replace(text,expression,f'l3_fault!("{point}", {expression})')
    text=replace(text,'rename_noreplace_raw(&a, &b)','l3_fault!("rename", rename_noreplace_raw(&a, &b))',2)
    tests='\n#[cfg(all(test, unix))]\nmod l3_premises {\nuse super::*;\n'
    for point in ['create','rename','recreate','fsync']:
        tests+=f'''#[test]
fn preflight_preserves_native_storage_error_{point}() {{
    crate::test_isolated::enter("host::volume::l3_premises::preflight_preserves_native_storage_error_{point}");
    for code in [libc::ENOSPC, libc::EDQUOT] {{
        let dir = tempfile::tempdir().unwrap();
        L3_FAULT.with(|fault| *fault.borrow_mut() = Some(("{point}", code)));
        let error = preflight_volume(dir.path()).unwrap_err();
        let got = match error {{ Error::Io(e) => e.raw_os_error(), _ => None }};
        assert_eq!(got, Some(code), "L3 P1 preflight-{point}: original native code was erased");
    }}
    println!("L3 P1 preflight-{point} native-codes=ok");
}}
'''
    tests+='''#[test]
fn nonstorage_errors_keep_severity() {
    crate::test_isolated::enter("host::volume::l3_premises::nonstorage_errors_keep_severity");
    for code in [libc::EIO, libc::EACCES] {
        let dir = tempfile::tempdir().unwrap();
        L3_FAULT.with(|fault| *fault.borrow_mut() = Some(("create", code)));
        let error = preflight_volume(dir.path()).unwrap_err();
        assert!(matches!(error, Error::Io(e) if e.kind() == std::io::ErrorKind::Unsupported));
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(preflight_refusal(dir.path(), format_args!("incompatible volume")), Error::Io(e) if e.kind() == std::io::ErrorKind::Unsupported));
    assert!(matches!(crate::Error::State("malformed state".into()), crate::Error::State(_)));
    println!("L3 P1 volume nonstorage-controls=ok");
}
}
'''
    return text+tests

def reset_overlay(text):
    text=replace(text,'use crate::supervisor::*;','use crate::supervisor::*;\n'+MACRO)
    operations={
        'flush':'crate::host::fsync_file(&live)',
        'rename':'crate::host::publish_noreplace(&live, &aside)',
        'create':'std::fs::create_dir_all(voyages_dir(state_dir))',
        'bootstrap':'VoyageStore::bootstrap(&root, new_voyage, RetentionClass::Archive)',
        'pointer':'pointer::publish(state_dir, new_voyage)',
    }
    for point,expression in operations.items():
        text=replace(text,expression,f'l3_fault!("{point}", {expression})')
    tests='\n#[cfg(test)]\nmod l3_premises {\nuse super::*;\n'
    for point in operations:
        tests+=f'''#[test]
fn reset_preserves_native_storage_error_{point}() {{
    crate::test_isolated::enter("supervisor::journal::reset::l3_premises::reset_preserves_native_storage_error_{point}");
    #[cfg(unix)] let codes = [libc::ENOSPC, libc::EDQUOT];
    #[cfg(windows)] let codes = [windows_sys::Win32::Foundation::ERROR_DISK_FULL as i32, windows_sys::Win32::Foundation::ERROR_HANDLE_DISK_FULL as i32];
    for code in codes {{
        let dir = tempfile::tempdir().unwrap();
        discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        L3_FAULT.with(|fault| *fault.borrow_mut() = Some(("{point}", code)));
        let error = reset_pointer(dir.path(), &uuid::Uuid::now_v7().to_string(), None).unwrap_err();
        let got = match error {{ crate::Error::Io(e) => e.raw_os_error(), _ => None }};
        assert_eq!(got, Some(code), "L3 P1 reset-{point}: original native code was erased");
    }}
    println!("L3 P1 reset-{point} native-codes=ok");
}}
'''
    tests+='''#[test]
fn nonstorage_errors_keep_severity() {
    crate::test_isolated::enter("supervisor::journal::reset::l3_premises::nonstorage_errors_keep_severity");
    let dir = tempfile::tempdir().unwrap();
    discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    #[cfg(unix)] let code = libc::EIO;
    #[cfg(windows)] let code = 1117;
    L3_FAULT.with(|fault| *fault.borrow_mut() = Some(("flush", code)));
    assert!(matches!(reset_pointer(dir.path(), &uuid::Uuid::now_v7().to_string(), None).unwrap_err(), crate::Error::State(_)));
    println!("L3 P1 reset nonstorage-controls=ok");
}
}
'''
    return text+tests

WINDOWS_TESTS=r'''
#[cfg(all(test, windows))]
mod l3_premises {
    use super::*;
    #[test]
    fn windows_context_preserves_full_codes_disk() {
        crate::test_isolated::enter("host::l3_premises::windows_context_preserves_full_codes_disk");
        check(windows_sys::Win32::Foundation::ERROR_DISK_FULL as i32);
    }
    #[test]
    fn windows_context_preserves_full_codes_handle() {
        crate::test_isolated::enter("host::l3_premises::windows_context_preserves_full_codes_handle");
        check(windows_sys::Win32::Foundation::ERROR_HANDLE_DISK_FULL as i32);
    }
    fn check(code: i32) {
        let error = io_ctx(std::io::Error::from_raw_os_error(code), format_args!("premise"));
        let got = match error { Error::Io(e) => e.raw_os_error(), _ => None };
        assert_eq!(got, Some(code), "L3 P1 Windows context erased the native full-volume code");
        println!("L3 P1 windows-context code={code} raw-preserved=ok");
    }
}
'''
STORAGE=r'''//! Prospective C1 native code table, used only in a disposable source copy.
use crate::Error;
use crate::lane::transport::TransportError;
pub fn storage_exhaustion(error: &Error) -> Option<i32> {
    let io = match error {
        Error::Io(source) => source,
        Error::Transport(TransportError::RuntimeDir(source) | TransportError::Io { source, .. }) => source,
        #[cfg(windows)] Error::Conpty(error) => &error.source,
        _ => return None,
    };
    let code = io.raw_os_error()?;
    #[cfg(unix)] let codes = [libc::ENOSPC, libc::EDQUOT];
    #[cfg(windows)] let codes = [windows_sys::Win32::Foundation::ERROR_DISK_FULL as i32, windows_sys::Win32::Foundation::ERROR_HANDLE_DISK_FULL as i32];
    codes.contains(&code).then_some(code)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonstorage_errors_keep_severity() {
        assert_eq!(storage_exhaustion(&Error::State("malformed state".into())), None);
        assert_eq!(storage_exhaustion(&Error::Io(std::io::Error::new(std::io::ErrorKind::Unsupported, "incompatible volume"))), None);
        #[cfg(unix)] let codes = [libc::EIO, libc::EACCES];
        #[cfg(windows)] let codes = [5, 1117];
        for code in codes {
            assert_eq!(storage_exhaustion(&Error::Io(std::io::Error::from_raw_os_error(code))), None);
        }
        println!("L3 P1 classifier nonstorage-controls=ok");
    }
}
'''

def call_end(text,start):
    # Calls in the anchored contextual boundaries are balanced; account for strings
    # so a path or diagnostic parenthesis cannot move the selected closure boundary.
    depth=0; quoted=False; escape=False
    for i in range(text.index('(',start),len(text)):
        c=text[i]
        if quoted:
            if escape: escape=False
            elif c=='\\': escape=True
            elif c=='"': quoted=False
        elif c=='"': quoted=True
        elif c=='(': depth+=1
        elif c==')':
            depth-=1
            if depth==0:return i+1
    raise AssertionError('unclosed premise source anchor')

def preserve_context(text,call,kind):
    cursor=0;changed=0
    while True:
        start=text.find(call+'(',cursor)
        if start<0:break
        end=call_end(text,start)
        old=text[start:end]
        if '{e}' not in old:
            cursor=end;continue
        if kind=='volume':
            detail=old[old.index('format_args!(')+len('format_args!'): -1].strip().rstrip(',')
            detail='format!'+detail
            fallback='preflight_refusal(dir, format_args!("{detail}"))'
        else:
            detail=old[len('err_state('):-1]
            fallback='err_state(detail)'
        new='{ let detail = '+detail+'; let native = crate::Error::from(e); if crate::host::storage_exhaustion(&native).is_some() { native } else { '+fallback+' } }'
        text=text[:start]+new+text[end:];cursor=start+len(new);changed+=1
    assert changed == (5 if kind=='volume' else 5), 'context inventory changed'
    return text

def prospective(root):
    host=root/'rust/log/src/host/mod.rs'
    text=host.read_text()
    text=replace(text,'mod volume;','mod volume;\nmod storage;\npub use storage::storage_exhaustion;')
    text=replace(text,'    let code = match e.raw_os_error() {','    let native = Error::Io(e);\n    if storage_exhaustion(&native).is_some() { return native; }\n    let Error::Io(e) = native else { unreachable!() };\n    let code = match e.raw_os_error() {')
    host.write_text(text)
    (host.parent/'storage.rs').write_text(STORAGE)
    volume=root/'rust/log/src/host/volume.rs'
    text=preserve_context(volume.read_text(),'preflight_refusal','volume')
    old='return Err(Error::Io(std::io::Error::new(e.kind(), format!("statfs {dir:?}: {e}"))));'
    new='let native = Error::Io(e);\n        if crate::host::storage_exhaustion(&native).is_some() { return Err(native); }\n        let Error::Io(e) = native else { unreachable!() };\n        '+old
    text=replace(text,old,new,2)
    volume.write_text(text)
    reset=root/'rust/log/src/supervisor/journal/reset.rs'
    reset.write_text(preserve_context(reset.read_text(),'err_state','reset'))
    start=root/'rust/log/src/capsule/writer_loop/start.rs'
    text=start.read_text()
    text=replace(text,'transport.0.bind(&config.voyage_id)?;','transport.0.bind(&config.voyage_id).map_err(l3_native_io)?;')
    text=replace(text,'std::result::Result<P, String>','Result<P>')
    text=replace(text,'        Err(format!(\n            "initial geometry','        Err(Error::State(format!(\n            "initial geometry')
    text=replace(text,'            config.cols, config.rows\n        ))','            config.cols, config.rows\n        )))')
    text=replace(text,'P::spawn(&config.argv, config.cols, config.rows).map_err(|e| e.to_string())','P::spawn(&config.argv, config.cols, config.rows).map_err(l3_native_io)')
    text=replace(text,'Err(reason) => return seal_spawn_failure(reason, ctx, w, &mut store, frames_written, segments_sealed)\n            .map(ControlFlow::Break),',
        'Err(reason) => {\n            if crate::host::storage_exhaustion(&reason).is_some() { return Err(reason); }\n            return seal_spawn_failure(reason.to_string(), ctx, w, &mut store, frames_written, segments_sealed).map(ControlFlow::Break);\n        }')
    text+='''\nfn l3_native_io(error: Error) -> Error {
    if crate::host::storage_exhaustion(&error).is_none() { return error; }
    match error {
        Error::Io(source) => Error::Io(source),
        Error::Transport(crate::lane::transport::TransportError::RuntimeDir(source) | crate::lane::transport::TransportError::Io { source, .. }) => Error::Io(source),
        #[cfg(windows)] Error::Conpty(error) => Error::Io(error.source),
        _ => unreachable!(),
    }
}
'''
    start.write_text(text)

for label in ['parent','head','revert']:
    root=destination/label
    assert not root.exists(), 'refusing to reuse a premise source snapshot'
    root.mkdir()
    with zipfile.ZipFile(io.BytesIO(archive)) as z:
        z.extractall(root)
    # The temporary branch carries only these tests and their folder records.
    source=git_repo/'rust/log/tests/fault_storage'
    target=root/'rust/log/tests/fault_storage'
    target.mkdir(exist_ok=True)
    for path in source.iterdir():
        if path.is_file(): (target/path.name).write_bytes(path.read_bytes())
    boundary=target/'boundaries.rs'
    text=boundary.read_text()
    for name in ['spawn_and_bind_preserve_native_storage_error','nonstorage_errors_keep_severity','real_full_volume_before_ready','real_full_volume_after_output_starts','harness_real_body','harness_assertion_failure','harness_duplicate_entry']:
        # A Linux proof driver itself lives in an already registered parent copy.
        text=text.replace('#[test]\npub fn '+name+'(', 'pub fn '+name+'(')
        text=replace(text,'pub fn '+name+'(', '#[test]\npub fn '+name+'(')
    boundary.write_text(text)
    for rel,overlay in [('rust/log/src/host/volume.rs',volume_overlay),('rust/log/src/supervisor/journal/reset.rs',reset_overlay)]:
        p=root/rel;p.write_text(overlay(p.read_text()))
    p=root/'rust/log/src/host/mod.rs';p.write_text(p.read_text()+WINDOWS_TESTS)
    helper=root/'rust/log/src/test_isolated.rs'
    text=helper.read_text()
    for name in ['a_real_body_enters_exactly_once','an_entry_dropped_unchecked_fails','a_child_that_fills_its_pipe_is_drained_and_finishes','a_stalled_child_fails_at_its_bound','output_a_descendant_holds_fails_at_the_bound']:
        full='test_isolated::tests::'+name
        text=replace(text,'fn '+name+'() {','fn '+name+'() {\n        if std::env::var(CHILD).as_deref() != Ok("'+full+'") { enter("'+full+'"); }')
    helper.write_text(text)
    if label!='parent': prospective(root)
    if label=='revert':
        # R1 bypasses only preservation/normalization branches; all definitions stay.
        for rel in ['rust/log/src/host/mod.rs','rust/log/src/host/volume.rs','rust/log/src/supervisor/journal/reset.rs','rust/log/src/capsule/writer_loop/start.rs']:
            p=root/rel
            text=p.read_text()
            text=text.replace('if storage_exhaustion(&native).is_some()', 'if false')
            text=text.replace('if crate::host::storage_exhaustion(&native).is_some()', 'if false')
            text=text.replace('if crate::host::storage_exhaustion(&reason).is_some()', 'if false')
            text=text.replace('if crate::host::storage_exhaustion(&error).is_none()', 'if true')
            p.write_text(text)
    hashes={str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob('*.rs')}
    manifest=json.dumps(hashes,sort_keys=True,indent=2)+'\n'
    (root/'source-hashes.json').write_text(manifest)
    print('L3 P1 snapshot='+label+' premise-head='+HEAD+' base='+BASE+' rust-source-manifest-sha256='+hashlib.sha256(manifest.encode()).hexdigest()+' git-archive-sha256='+hashlib.sha256(archive).hexdigest())
print('L3 premise snapshots parent/head/revert prepared')
"###;
