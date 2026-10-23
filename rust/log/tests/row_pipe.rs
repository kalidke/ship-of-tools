#![cfg(windows)]
//! The row socket on a Windows pipe (proven on the hosted run): every instance the listener creates carries a
//! protected DACL with one allow entry, for the token user. Several clients are held open at once so the listener
//! creates further instances, and each one's descriptor is read back from the client's own handle; each also answers
//! the client's challenge and the hello.

use sot_log::row::{row_socket_path, RowClient, RowRun, RowServer};
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::time::Duration;
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
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
    let raw: Vec<std::fs::File> = (0..4)
        .map(|_| std::fs::OpenOptions::new().read(true).write(true).open(&path).expect("open an instance"))
        .collect();
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
