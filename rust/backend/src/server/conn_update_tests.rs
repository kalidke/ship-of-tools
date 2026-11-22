// The state of the daemon's update through a real served connection: the hello's field and the `update.state` event
// read memory alone, and only a window's hello begins a reading of the disk.
use super::*;
use crate::update::{Chase, Status};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

type Half<T> = tokio::io::BufReader<tokio::io::ReadHalf<T>>;

/// One connection served over `status`, its hello said as `role` and `name`.
struct Served {
    rx: Half<tokio::io::DuplexStream>,
    tx: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    /// The hello reply's payload.
    hello: serde_json::Value,
    _root: tempfile::TempDir,
}

async fn serve(status: &Status, role: &str, name: Option<&str>) -> Served {
    let root = tempfile::tempdir().unwrap();
    let services = Services { update: status.clone(), ..Services::for_test("update", root.path()) };
    let (client, server) = tokio::io::duplex(1 << 16);
    let (srx, stx) = tokio::io::split(server);
    tokio::spawn(handle_connection(srx, stx, services, None));
    let (crx, mut ctx) = tokio::io::split(client);
    let mut rx = codec::buffered(crx);
    let hello = json!({
        "client_id": "update-client", "protocol": sot_protocol::PROTOCOL_VERSION, "app_version": sot_protocol::app_version(),
        "host": "test-host", "os_user": "test-user", "role": role, "name": name,
    });
    codec::write_frame(&mut ctx, &Frame::req(1, op::HELLO, hello), None).await.unwrap();
    let answered = tokio::time::timeout(Duration::from_millis(900), codec::read_frame(&mut rx)).await;
    let (reply, _) = answered.expect("a hello answered sooner than a reading's bound, whatever the disk does").unwrap();
    assert_eq!(reply.op, op::HELLO, "{reply:?}");
    Served { rx, tx: ctx, hello: reply.payload, _root: root }
}

impl Served {
    /// The next `update.state` event's payload, read within a bound shorter than a reading's.
    async fn told(&mut self) -> serde_json::Value {
        let next = async {
            loop {
                let (frame, _) = codec::read_frame(&mut self.rx).await.unwrap();
                if frame.kind == Kind::Evt && frame.op == op::UPDATE_STATE {
                    return frame.payload;
                }
            }
        };
        tokio::time::timeout(Duration::from_millis(900), next).await.expect("an update.state event")
    }

    /// Whether a `ping` sent now is answered within half a reading's bound.
    async fn answers(&mut self) -> bool {
        codec::write_frame(&mut self.tx, &Frame::req(2, op::PING, json!({})), None).await.unwrap();
        let pong = async {
            loop {
                let (frame, _) = codec::read_frame(&mut self.rx).await.unwrap();
                if frame.kind == Kind::Res && frame.op == op::PING {
                    return;
                }
            }
        };
        tokio::time::timeout(Duration::from_millis(500), pong).await.is_ok()
    }
}

/// A disk whose every reading hangs until `0` is dropped, and the count of readings begun.
struct Hung(std::sync::mpsc::Sender<()>, Arc<AtomicUsize>);

/// An owner over that disk: a reading let go reads the folder `updates`, or finds nothing armed with none.
fn hung(updates: Option<std::path::PathBuf>) -> (Status, Hung) {
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (held, begun) = (std::sync::Mutex::new(held), Arc::new(AtomicUsize::new(0)));
    let counted = begun.clone();
    let status = Status::new(Chase::Unchecked, false, move || {
        counted.fetch_add(1, Ordering::SeqCst);
        let _ = held.lock().unwrap().recv();
        updates.clone().map(|updates| crate::update::Disk { updates, target: "linux-x86_64".into(), unit: false })
    });
    (status, Hung(release, begun))
}

async fn until(what: &str, check: impl Fn() -> bool) {
    let began = std::time::Instant::now();
    while !check() {
        assert!(began.elapsed() < Duration::from_secs(5), "{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A hello carries the state the owner holds in memory and a change is told as an event, both while a reading of the
/// disk hangs: neither waits for it, and neither says anything of the disk (the `unread` mark is `version.query`'s
/// alone, of a reading that answer waited for). A reading that ends having changed nothing is told to nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hello_and_an_event_tell_the_state_in_memory_while_the_disk_hangs() {
    let (status, Hung(release, begun)) = hung(None);
    let reading = tokio::spawn({
        let status = status.clone();
        async move { status.reconcile().await }
    });
    until("the reading began", || begun.load(Ordering::SeqCst) == 1).await;

    let mut conn = serve(&status, "cli", None).await;
    assert_eq!(conn.hello["update"], json!({"phase": "unchecked"}), "{}", conn.hello);
    status.set_chase(Chase::Preparing { version: "0.7.4".into() });
    assert_eq!(conn.told().await, json!({"phase": "preparing", "version": "0.7.4"}));
    // A second connection reads the same state in its hello, and is told nothing it was already told there.
    let mut later = serve(&status, "cli", None).await;
    assert_eq!(later.hello["update"], json!({"phase": "preparing", "version": "0.7.4"}));
    assert_eq!(begun.load(Ordering::SeqCst), 1, "a hello or an event began a reading");

    assert!(!reading.await.unwrap(), "the reading hung past its bound");
    drop(release);
    assert!(status.reconcile().await, "the readings end once the disk answers");
    // The two connections were told nothing by the readings' end: the next thing each is told is the next change.
    status.set_chase(Chase::None);
    for conn in [&mut conn, &mut later] {
        assert_eq!(conn.told().await, json!({"phase": "none"}));
    }
}

/// A window's hello begins one reading of the armed update, and neither its reply nor the connection after it waits
/// for that reading; what the reading finds armed reaches the window as an event. No other client's hello begins one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_windows_hello_begins_a_reading_and_no_other_hello_does() {
    // An update another program armed, which this daemon has not read.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pending-linux-x86_64.json"),
        format!(
            r#"{{"schema":1,"repo":"example/project","tag":"v9.9.9","version":"9.9.9","target":"linux-x86_64",
                "asset":"sot-9.9.9-linux-x86_64.tar.gz","asset_sha256":"{}","checkout":"/x","commit":"cc","armed_at":1}}"#,
            "ab".repeat(32)
        ),
    )
    .unwrap();
    let (status, Hung(release, begun)) = hung(Some(dir.path().to_path_buf()));
    for (role, name) in [("cli", Some("someone")), ("agent", None), ("fe", None)] {
        let conn = serve(&status, role, name).await;
        assert_eq!(conn.hello["update"], json!({"phase": "unchecked"}), "{role}: {}", conn.hello);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(begun.load(Ordering::SeqCst), 0, "a hello that is no window's began a reading");

    let mut window = serve(&status, "fe", Some("fe@test-host")).await;
    assert_eq!(window.hello["update"], json!({"phase": "unchecked"}), "answered before the reading ends");
    until("the window's hello began a reading", || begun.load(Ordering::SeqCst) == 1).await;
    assert!(window.answers().await, "the window's connection waited on the reading its hello began");
    drop(release);
    let told = window.told().await;
    assert_eq!((&told["phase"], &told["version"], told.get("unread")), (&json!("ready"), &json!("9.9.9"), None), "{told}");
}
