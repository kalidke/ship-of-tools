//! Snapshot preparation is a private premise harness, not a product recovery owner.
use super::*;

pub fn boundary_gate() {
    // Boundary cases become selected tests only in the archived source copies.
    // The outer workspace suite runs this driver and P2; expected parent reds
    // belong to the driver, rather than making the CI premise job itself fail.
    let _cases = [
        boundaries::spawn_and_bind_preserve_native_storage_error as fn(),
        boundaries::nonstorage_errors_keep_severity,
        boundaries::real_full_volume_before_ready,
        boundaries::real_full_volume_after_output_starts,
    ];
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let dir = tempfile::tempdir_in(scratch()).unwrap();
    let mut prepare = Command::new("python3");
    prepare.arg("-c").arg(PREPARE).arg(repo).arg(dir.path());
    let (code, text) = bounded(prepare, 120);
    assert_eq!(code, 0, "premise snapshot preparation failed:\n{text}");
    println!("{text}");
    for label in ["parent", "head", "revert"] {
        for (target, name, red) in selections() {
            let expected = if label == "head" || !red { 0 } else { 101 };
            let command = cargo_selection(dir.path(), label, target, name);
            let (code, text) = bounded(command, 600);
            let logs = scratch().parent().unwrap().join("logs");
            std::fs::create_dir_all(&logs).unwrap();
            std::fs::write(
                logs.join(format!("l3-P1-{label}-{}.log", name.replace("::", "-"))),
                &text,
            )
            .unwrap();
            println!("{text}");
            println!("P1-{label}-{name} exit {code} expected {expected}");
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
            }
        }
    }
    println!("L3 P1 parent=red prospective=green reversal=red controls=green");
}

fn cargo_selection(root: &Path, label: &str, target: &str, name: &str) -> Command {
    let mut command = Command::new("nice");
    command.arg("cargo");
    command.current_dir(root.join(label).join("rust"));
    command
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_PROFILE_DEV_DEBUG", "line-tables-only");
    #[cfg(windows)]
    if label != "head" && name == "boundaries::real_full_volume_before_ready" {
        command.env("L3_WINDOWS_PARENT_OBSERVATION", "1");
    }
    // Archives outside a checkout have no git identity. Use their exact
    // source-manifest digest, so each prospective build has its own id.
    use sha2::Digest;
    let manifest = std::fs::read(root.join(label).join("source-hashes.json")).unwrap();
    let digest = format!("{:x}", sha2::Sha256::digest(manifest));
    command.env(
        "SOT_BUILD_ID",
        format!("l3-premise-{label}-{}", &digest[..40]),
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only this owned child's mask changes.
        unsafe {
            command.pre_exec(|| {
                libc::umask(0o022);
                Ok(())
            });
        }
    }
    command.args(["test", "-p", "sot-log", "--locked", "-j", "8"]);
    // Keep the caller's CARGO_TARGET_DIR. A subdirectory avoids the outer
    // workspace test's cargo lock when CI builds these disposable copies.
    command
        .arg("--target-dir")
        .arg(scratch().join("l3-premise-build").join(label));
    if target == "lib" {
        command.arg("--lib");
    } else {
        command.args(["--test", "fault_storage"]);
    }
    command
        .arg(name)
        .args(["--", "--exact", "--nocapture", "--test-threads=1"]);
    command
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
BASE = 'bde3b3e0066ed2d26af5e26d48cc6bcd5a76a0ab'
repo, destination = map(Path, sys.argv[1:3])
product = subprocess.check_output(['git','diff',BASE,'HEAD','--','rust/log/src'],cwd=repo)
assert not product, 'premise branch must contain no product edits'
archive = subprocess.check_output(['git','archive','--format=zip','HEAD','rust'],cwd=repo)
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
        check(windows_sys::Win32::Foundation::ERROR_DISK_FULL as i32);
    }
    #[test]
    fn windows_context_preserves_full_codes_handle() {
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
    source=repo/'rust/log/tests/fault_storage'
    target=root/'rust/log/tests/fault_storage'
    target.mkdir(exist_ok=True)
    for path in source.iterdir():
        if path.is_file(): (target/path.name).write_bytes(path.read_bytes())
    boundary=target/'boundaries.rs'
    text=boundary.read_text()
    for name in ['spawn_and_bind_preserve_native_storage_error','nonstorage_errors_keep_severity','real_full_volume_before_ready','real_full_volume_after_output_starts']:
        text=replace(text,'pub fn '+name+'(', '#[test]\npub fn '+name+'(')
    boundary.write_text(text)
    for rel,overlay in [('rust/log/src/host/volume.rs',volume_overlay),('rust/log/src/supervisor/journal/reset.rs',reset_overlay)]:
        p=root/rel;p.write_text(overlay(p.read_text()))
    p=root/'rust/log/src/host/mod.rs';p.write_text(p.read_text()+WINDOWS_TESTS)
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
    (root/'source-hashes.json').write_text(json.dumps(hashes,sort_keys=True,indent=2)+'\n')
print('L3 premise snapshots parent/head/revert prepared')
"###;
