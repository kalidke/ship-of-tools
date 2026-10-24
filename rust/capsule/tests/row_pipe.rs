#![cfg(windows)]
//! The row socket on a Windows pipe (proven on the hosted run): every instance the listener creates carries a
//! protected DACL with one allow entry, for the token user. Several clients are held open at once so the listener
//! creates further instances, and each one's descriptor is read back from the client's own handle; each also answers
//! the client's challenge and the hello.

use sot_capsule::row::{row_socket_path, RowClient, RowRun, RowServer};
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::time::Duration;
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, GetTokenInformation, AclSizeInformation,
    ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, TokenUser,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The token user's SID, in a buffer that outlives its use.
fn token_user() -> Vec<u8> {
    let mut token: HANDLE = std::ptr::null_mut();
    assert_ne!(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }, 0);
    let mut len = 0u32;
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    let mut buf = vec![0u8; len as usize];
    assert_ne!(unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) }, 0);
    buf
}

/// Asserts the pipe instance behind `handle` has a protected DACL of one entry, an allow for `owner`.
fn assert_owner_only(handle: HANDLE, owner: PSID) {
    let mut dacl = std::ptr::null_mut();
    let mut sd = std::ptr::null_mut();
    let rc = unsafe {
        GetSecurityInfo(handle, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd)
    };
    assert_eq!(rc, 0, "GetSecurityInfo on a pipe instance");
    let (mut control, mut revision) = (0u16, 0u32);
    assert_ne!(unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) }, 0);
    assert_ne!(control & SE_DACL_PROTECTED, 0, "the DACL is protected: nothing is inherited into it");
    let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    assert_ne!(
        unsafe { GetAclInformation(dacl, (&mut info as *mut ACL_SIZE_INFORMATION).cast(), std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation) },
        0
    );
    assert_eq!(info.AceCount, 1, "one entry grants access: the owner's");
    let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
    assert_ne!(unsafe { GetAce(dacl, 0, &mut ace) }, 0);
    let sid = unsafe { std::ptr::addr_of!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart) } as PSID;
    assert_ne!(unsafe { EqualSid(sid, owner) }, 0, "the entry is for the token user");
    unsafe { LocalFree(sd) };
}

#[test]
fn every_pipe_instance_is_owner_only() {
    let h = format!("test{}", std::process::id());
    let path = row_socket_path(&h).expect("the pipe name");
    let run = RowRun::new("run-one", "ready");
    let server = RowServer::bind(&path, Arc::clone(&run)).expect("bind the pipe");
    let user = token_user();
    let owner = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    // Four connections held at once force the listener to create further instances after the first; each is read back.
    // The listener creates the next instance only after it accepted the one before, so an open that finds every
    // instance busy tries again, as the lane's pipe client does (bounded).
    let open = || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                r => break r.expect("open an instance"),
            }
        }
    };
    let raw: Vec<std::fs::File> = (0..4).map(|_| open()).collect();
    for f in &raw {
        assert_owner_only(f.as_raw_handle() as HANDLE, owner);
    }
    drop(raw);
    // And each instance, whole, answers the client's own challenge and the hello.
    let clients: Vec<RowClient> = (0..4)
        .map(|_| RowClient::connect(&path, "test", Duration::from_secs(10)).expect("each instance answers"))
        .collect();
    assert!(clients.iter().all(|c| c.run() == "run-one"));
    drop(clients);
    server.shutdown();
}

/// A peer that sends `row.stop` and never reads its reply cannot hold the shutdown: the writer's flush on a pipe blocks
/// until the peer reads, and the connection stays registered until it ends, so `close_all` cancels that flush.
#[test]
fn a_peer_that_never_reads_its_stop_reply_cannot_hold_the_shutdown() {
    use std::io::Write;
    struct Stopper;
    impl sot_capsule::row::Management for Stopper {
        fn status(&self) -> sot_capsule::row::StatusRes {
            sot_capsule::row::StatusRes { phase: "ready".into(), run: None }
        }
        fn end_run(&self, _: &str, _: &str) -> sot_capsule::row::EndRunRes {
            unreachable!()
        }
        fn reset(&self) -> sot_capsule::row::ResetRes {
            unreachable!()
        }
        fn stop(&self, _: sot_capsule::row::ConnId) -> sot_capsule::row::StopRes {
            sot_capsule::row::StopRes { outcome: sot_capsule::row::StopOutcome::Stopping }
        }
        fn conn_gone(&self, _: sot_capsule::row::ConnId) {}
    }
    let h = format!("stop{}", std::process::id());
    let path = row_socket_path(&h).expect("the pipe name");
    let run = RowRun::new("run-one", "ready");
    run.set_management(Box::new(Stopper));
    let server = RowServer::bind(&path, run).expect("bind the pipe");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut peer = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            r => break r.expect("open an instance"),
        }
    };
    for (id, op, payload) in [
        (1, "row.hello", serde_json::json!({"build": "t", "row": sot_capsule::row::ROW_PROTOCOL})),
        (2, "row.stop", serde_json::json!({})),
    ] {
        let mut text = serde_json::to_vec(&sot_protocol::Frame::req(id, op, payload)).unwrap();
        text.push(b'\n');
        peer.write_all(&text).unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        server.shutdown();
        let _ = done.send(());
    });
    assert!(ended.recv_timeout(Duration::from_secs(10)).is_ok(), "the shutdown never ended with a peer that does not read");
    drop(peer);
}
