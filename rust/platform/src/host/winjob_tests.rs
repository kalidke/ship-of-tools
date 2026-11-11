use super::*;
use crate::test_isolated::wait_within;
use std::process::{Command, Stdio};
use std::time::Duration;

/// P9: a child assigned after `spawn` ends when the job's handle closes, also when this process is itself in a job
/// (a hosted runner's is; the line says which).
#[test]
fn an_assigned_child_ends_with_its_job() {
    let job = AnonymousJob::create().unwrap();
    // `ping -n 60` runs for a minute unless it is ended.
    let mut child = Command::new("ping")
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    job.assign(&child).unwrap();
    let mut mine = 0;
    // SAFETY: a null job asks whether the process is in any job.
    unsafe { windows_sys::Win32::System::JobObjects::IsProcessInJob(windows_sys::Win32::System::Threading::GetCurrentProcess(), std::ptr::null_mut(), &mut mine) };
    println!("the test process is itself in a job: {}", mine != 0);
    assert_eq!(job.active_processes().unwrap(), 1);
    assert!(child.try_wait().unwrap().is_none());
    let closed = std::time::Instant::now();
    drop(job);
    wait_within(&mut child, Duration::from_secs(20));
    println!("the child ended {:?} after the job's handle closed", closed.elapsed());
}
