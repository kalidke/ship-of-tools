//! P1 calls the actual run and contextual boundaries; only the fixtures inject native IO.
use super::*;
use sot_log::capsule::producer::{ExitStatus, Producer};
use sot_log::capsule::{self, CapsuleConfig};
use sot_log::lane::attach_proto::ConnId;
use sot_log::lane::transport::{Transport, TransportError, TransportEvent};
use sot_log::lane::wire::Survival;
use sot_log::store::segment::RetentionClass;
use sot_log::{Error, Result};
use std::io::Write;
use std::sync::mpsc;
use std::time::Instant;

pub fn codes() -> Vec<i32> {
    #[cfg(unix)]
    {
        vec![libc::ENOSPC, libc::EDQUOT]
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL};
        vec![ERROR_DISK_FULL as i32, ERROR_HANDLE_DISK_FULL as i32]
    }
}

fn assert_io(error: Error, expected: i32, label: &str) {
    let code = match error {
        Error::Io(e) => e.raw_os_error(),
        _ => None,
    };
    assert_eq!(
        code,
        Some(expected),
        "L3 P1 {label}: original Error::Io/raw_os_error must reach run caller"
    );
}

fn config(root: &Path, argv: Vec<String>) -> CapsuleConfig {
    CapsuleConfig {
        voyage_root: root.join("voyage"),
        voyage_id: "l3-premise".into(),
        retention: RetentionClass::Discard,
        producer_kind: "premise".into(),
        argv,
        cols: 80,
        rows: 25,
        survival: Survival::Normal,
        parent_lease: None,
        rollout_evidence: sot_log::store::rollout::RolloutEvidence::NoRollbackTarget,
    }
}

fn source(kind: &str, code: i32) -> Error {
    let source = std::io::Error::from_raw_os_error(code);
    match kind {
        "io" => Error::Io(source),
        "transport" => Error::Transport(TransportError::Io {
            op: "premise",
            source,
        }),
        "runtime" => Error::Transport(TransportError::RuntimeDir(source)),
        #[cfg(windows)]
        "conpty" => Error::Conpty(sot_log::capsule::producer::conpty::ConptyError {
            op: "premise",
            source,
        }),
        _ => panic!("unknown native source"),
    }
}

struct FaultTransport {
    fault: Option<Error>,
}
impl Transport for FaultTransport {
    fn bind(&mut self, _: &str) -> Result<()> {
        self.fault.take().map_or(Ok(()), Err)
    }
    fn try_recv_event(&mut self) -> Option<TransportEvent> {
        None
    }
    fn send(&mut self, _: ConnId, _: Vec<u8>) -> u64 {
        0
    }
    fn close(&mut self, _: ConnId) {}
    fn shutdown_all(&mut self, _: Instant) -> bool {
        true
    }
}

struct FaultProducer;
impl Producer for FaultProducer {
    type Output = std::io::Empty;
    fn pre_spawn_detail() -> serde_json::Value {
        serde_json::json!({})
    }
    fn spawn(argv: &[String], _: u16, _: u16) -> Result<Self> {
        Err(source(&argv[0], argv[1].parse().unwrap()))
    }
    fn take_output(&mut self) -> Self::Output {
        unreachable!()
    }
    fn input(&mut self) -> &mut dyn Write {
        unreachable!()
    }
    fn resize(&self, _: u16, _: u16) -> Result<()> {
        unreachable!()
    }
    fn wait(&self, _: Duration) -> Result<bool> {
        unreachable!()
    }
    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus> {
        unreachable!()
    }
    fn terminate_domain(&self) -> Result<()> {
        unreachable!()
    }
    fn domain_is_empty(&self) -> Result<bool> {
        unreachable!()
    }
    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        unreachable!()
    }
}

pub fn spawn_and_bind_preserve_native_storage_error() {
    for code in codes() {
        #[cfg(unix)]
        let kinds = vec!["io", "transport", "runtime"];
        #[cfg(windows)]
        let kinds = vec!["io", "transport", "runtime", "conpty"];
        for kind in kinds {
            let dir = tempfile::tempdir_in(scratch()).unwrap();
            let cfg = config(dir.path(), vec![kind.into(), code.to_string()]);
            let (_tx, rx) = mpsc::channel();
            let mut transport = FaultTransport {
                fault: Some(source(kind, code)),
            };
            assert_io(
                capsule::run::<FaultProducer>(cfg, rx, &mut transport).unwrap_err(),
                code,
                "bind",
            );
            let cfg = config(dir.path(), vec![kind.into(), code.to_string()]);
            let (_tx, rx) = mpsc::channel();
            let mut transport = FaultTransport { fault: None };
            let result = capsule::run::<FaultProducer>(cfg, rx, &mut transport);
            assert!(
                result.is_err(),
                "L3 P1 spawn: storage failure must not become sealed SpawnFailed success"
            );
            assert_io(result.unwrap_err(), code, "spawn");
        }
    }
    println!("L3 P1 typed-spawn-bind native-codes=ok");
}

pub fn nonstorage_errors_keep_severity() {
    #[cfg(unix)]
    let codes = [libc::EIO, libc::EACCES];
    #[cfg(windows)]
    let codes = [5, 1117];
    for code in codes {
        let dir = tempfile::tempdir_in(scratch()).unwrap();
        let (_tx, rx) = mpsc::channel();
        let cfg = config(dir.path(), vec!["io".into(), code.to_string()]);
        let summary =
            capsule::run::<FaultProducer>(cfg, rx, &mut FaultTransport { fault: None }).unwrap();
        assert_eq!(summary.exit_kind, capsule::ExitKind::SpawnFailed);
    }
    println!("L3 P1 nonstorage EIO-permission compensation=ok");
}

pub fn real_full_volume_before_ready() {
    volume::on_volume("boundaries::real_full_volume_before_ready", |root| {
        let code = volume::fill(root);
        let (_tx, rx) = mpsc::channel();
        let cfg = config(root, vec!["io".into(), code.to_string()]);
        let error = capsule::run::<FaultProducer>(cfg, rx, &mut FaultTransport { fault: None })
            .unwrap_err();
        #[cfg(unix)]
        assert_io(error, code, "before-ready");
        #[cfg(windows)]
        {
            let observed = match error {
                Error::Io(e) => e.raw_os_error(),
                _ => panic!("L3 P1 Windows startup must return its native IO boundary"),
            };
            // NTFS can fail at directory creation or a file write. Parent source
            // inspection is observational; the two io_ctx tests supply its reds.
            if std::env::var_os("L3_WINDOWS_PARENT_OBSERVATION").is_some() {
                println!("L3 P1 Windows parent-startup observed-raw-code={observed:?}");
                assert!(observed.is_none_or(|code| codes().contains(&code)));
            } else {
                assert!(
                    observed.is_some_and(|code| codes().contains(&code)),
                    "L3 P1 Windows startup lost its native storage source"
                );
            }
        }
        volume::free_and_sync(root);
        println!("L3 P1 real-before-ready fill-code={code} caller-inspected=ok cleanup=ok");
    });
}

struct OutputProducer {
    input: std::io::Sink,
}
impl Producer for OutputProducer {
    type Output = std::io::Repeat;
    fn pre_spawn_detail() -> serde_json::Value {
        serde_json::json!({})
    }
    fn spawn(argv: &[String], _: u16, _: u16) -> Result<Self> {
        volume::fill(Path::new(&argv[0]));
        Ok(Self {
            input: std::io::sink(),
        })
    }
    fn take_output(&mut self) -> Self::Output {
        std::io::repeat(b'o')
    }
    fn input(&mut self) -> &mut dyn Write {
        &mut self.input
    }
    fn resize(&self, _: u16, _: u16) -> Result<()> {
        Ok(())
    }
    fn wait(&self, _: Duration) -> Result<bool> {
        Ok(false)
    }
    fn exit_status_after_confirmed_exit(&self) -> Result<ExitStatus> {
        unreachable!()
    }
    fn terminate_domain(&self) -> Result<()> {
        Ok(())
    }
    fn domain_is_empty(&self) -> Result<bool> {
        Ok(true)
    }
    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(|| {})
    }
}

pub fn real_full_volume_after_output_starts() {
    volume::on_volume("boundaries::real_full_volume_after_output_starts", |root| {
        let (_tx, rx) = mpsc::channel();
        let cfg = config(root, vec![root.to_string_lossy().into_owned()]);
        let error = capsule::run::<OutputProducer>(cfg, rx, &mut FaultTransport { fault: None })
            .unwrap_err();
        let code = match &error {
            Error::Io(e) => e.raw_os_error().unwrap(),
            _ => panic!("L3 P1 output lost native IO"),
        };
        assert!(
            codes().contains(&code),
            "L3 P1 output must report native exhaustion"
        );
        volume::free_and_sync(root);
        println!("L3 P1 real-output native-code={code} caller=Error::Io cleanup=ok");
    });
}
