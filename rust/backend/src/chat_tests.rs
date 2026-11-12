use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sot_chat::config::{CONFIG_FILE, SECRETS_FILE};

use crate::lifecycle::retire::retire;
use sot_chat::service::Service;

pub(crate) const SELF: &str = "hub-box";
const BOT: &str = "xoxb-1111-2222-aaaa";
const APP: &str = "xapp-1-A111-2222-bbbb";
/// A bound no test relies on meeting: it only has to be long enough for a stop that finishes.
const LONG: Duration = Duration::from_secs(60);

/// Synthetic homes: a config home, a state home and the folders of rows, none of them the real ones.
pub(crate) struct Home {
    dir: tempfile::TempDir,
    pub(crate) env: Env,
}

impl Home {
    /// `hub` names the hub in hosts.toml (`None`: no hosts.toml at all).
    pub(crate) fn new(hub: Option<&str>) -> Home {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let env = Env::empty()
            .with("HOME", root.join("home"))
            .with("XDG_CONFIG_HOME", root.join("config"))
            .with("XDG_STATE_HOME", root.join("state"))
            .with("SOT_COMM_HOME", root.join("comm"))
            .with("LOCALAPPDATA", root.join("local"))
            .with("SOT_SELF_HOST", SELF);
        let home = Home { dir, env };
        std::fs::create_dir_all(home.config()).unwrap();
        if let Some(hub) = hub {
            home.list(hub);
        }
        home
    }

    /// hosts.toml naming `hub`; a computer that is not the hub is listed, as every real one is.
    pub(crate) fn list(&self, hub: &str) {
        let me = if hub == SELF { String::new() } else { format!("[host.{SELF}]\ndaemon = true\n") };
        self.write("hosts.toml", &format!("hub = \"{hub}\"\n[host.{hub}]\ndaemon = true\n{me}"));
    }

    /// The comm folder this home's daemon files into, under the temporary folder.
    pub(crate) fn comm(&self) -> PathBuf {
        crate::comm::home::comm_home(&self.env).unwrap()
    }

    pub(crate) fn config(&self) -> PathBuf {
        config::config_dir(&self.env).unwrap()
    }

    pub(crate) fn write(&self, name: &str, text: &str) {
        std::fs::write(self.config().join(name), text).unwrap();
    }

    /// A folder a row could have, in its canonical spelling.
    pub(crate) fn folder(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join("rows").join(name);
        std::fs::create_dir_all(&p).unwrap();
        canonical(&p).unwrap()
    }

    /// A `chat.toml` that maps the folders to `C0AAAA` and `C0BBBB` in turn and delivers nothing (no `mail_after`).
    pub(crate) fn chat_toml(&self, folders: &[String]) {
        self.toml(folders, None);
    }

    /// The same mappings with the hub's `mail_after` and each mapping's own, so the file delivers.
    pub(crate) fn delivering_toml(&self, folders: &[String]) {
        self.delivering_toml_after(folders, "1.000000");
    }

    /// [`Home::delivering_toml`] with the floor `after` for the hub and every mapping.
    pub(crate) fn delivering_toml_after(&self, folders: &[String], after: &str) {
        self.toml(folders, Some(after));
    }

    fn toml(&self, folders: &[String], delivers: Option<&str>) {
        let after = delivers.map(|t| format!("mail_after = \"{t}\"\n")).unwrap_or_default();
        let mut text = format!("workspace = \"T0AAAA\"\nchannels = [\"C0AAAA\", \"C0BBBB\"]\n{after}");
        for (f, c) in folders.iter().zip(["C0AAAA", "C0BBBB"]) {
            text += &format!("[[mapping]]\nfolder = '{f}'\nchannel = \"{c}\"\n{after}");
        }
        self.write(CONFIG_FILE, &text);
    }

    pub(crate) fn secrets(&self) {
        self.write(SECRETS_FILE, &format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT}\"\napp_token = \"{APP}\"\n"));
    }

    /// Both files, with one mapped row.
    pub(crate) fn configured(&self) -> PathBuf {
        let row = self.folder("a");
        self.chat_toml(&[row.to_string_lossy().into_owned()]);
        self.secrets();
        row
    }
}

/// The reader every open of a chat file goes through, recording each path.
struct Observed(RefCell<Vec<PathBuf>>);

impl Observed {
    fn new() -> Observed {
        Observed(RefCell::new(Vec::new()))
    }

    fn read(&self, p: &Path) -> std::io::Result<String> {
        self.0.borrow_mut().push(p.to_path_buf());
        config::read_file(p)
    }

    fn opened(&self) -> Vec<PathBuf> {
        self.0.borrow().clone()
    }
}

#[derive(Default)]
struct Fake {
    stops: AtomicUsize,
    hang: bool,
}

impl Listener for Fake {
    fn stop(&self, _within: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.stops.fetch_add(1, Ordering::SeqCst);
            if self.hang {
                std::future::pending::<()>().await;
            }
        })
    }
}

fn fact(canonical: &Path, inert: bool) -> RowFact {
    RowFact { canonical: canonical.to_path_buf(), inert }
}

fn gives(listener: &Arc<Fake>) -> Starter<Fake> {
    let listener = listener.clone();
    Box::new(move |_, _, _| {
        let listener = listener.clone();
        Box::pin(async move { Ok(listener) })
    })
}

fn refuses(code: &'static str) -> Starter<Fake> {
    Box::new(move |_, _, _| Box::pin(async move { Err(code) }))
}

/// A start that never finishes.
fn pending() -> Starter<Fake> {
    Box::new(|_, _, _| Box::pin(std::future::pending()))
}

/// A start that must not happen.
fn never(called: &Arc<AtomicBool>) -> Starter<Fake> {
    let called = called.clone();
    Box::new(move |_, _, _| {
        called.store(true, Ordering::SeqCst);
        Box::pin(async move { Err("unexpected") })
    })
}

fn begin(home: &Home, rows: &[RowFact], seen: &Observed, registry: &Registry, start: Starter<Fake>) -> Arc<Chat<Fake>> {
    Chat::begin(&home.env, rows, &|p| seen.read(p), registry, start, HUB_CHECK)
}

/// Yield until the start task has finished (no timer: it ends once its starter does).
async fn settled<L: Listener>(chat: &Chat<L>) -> Status {
    loop {
        match chat.status() {
            Status::Starting => tokio::task::yield_now().await,
            s => return s,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_hub_reads_chat_credentials_from_platform_config() {
    // Not the hub: another computer is, the topology is missing, or it does not parse. Both chat files are in every
    // home, and nothing opens either.
    let invalid = Home::new(None);
    invalid.write("hosts.toml", "hub = \n[[[");
    let others = [("another hub", Home::new(Some("elsewhere"))), ("no hosts.toml", Home::new(None)), ("invalid hosts.toml", invalid)];
    for (what, home) in &others {
        let row = home.configured();
        let (seen, registry, called) = (Observed::new(), Registry::new(), Arc::new(AtomicBool::new(false)));
        let chat = begin(home, &[fact(&row, false)], &seen, &registry, never(&called));
        assert_eq!(chat.status(), Status::NotHub, "{what}");
        assert!(!called.load(Ordering::SeqCst), "{what}");
        // The status and its words, the answer for a folder, a stop and a wait open nothing either.
        let _ = (chat.status().to_string(), chat.mapping(&row.to_string_lossy()), chat.service().is_none());
        chat.request_stop();
        assert!(chat.wait_retired(Duration::ZERO), "{what}");
        assert_eq!(seen.opened(), Vec::<PathBuf>::new(), "{what}: a non-hub opened a chat file");
        assert_eq!(registry.len(), 0, "{what}: a non-hub registered with lifecycle");
    }

    // The hub opens chat.toml and then chat-secrets.toml, both in platform's config folder, once each.
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (seen, listener) = (Observed::new(), Arc::new(Fake::default()));
    let chat = begin(&home, &[fact(&row, false)], &seen, &Registry::new(), gives(&listener));
    assert_eq!(settled(&chat).await, Status::Running);
    let both = vec![home.config().join(CONFIG_FILE), home.config().join(SECRETS_FILE)];
    assert_eq!(seen.opened(), both);

    // A hub that stops being the hub after its start (the topology names another, then is gone), and a stop, open
    // nothing more.
    home.list("elsewhere");
    assert_eq!(chat.status(), Status::Running);
    assert!(chat.service().is_some());
    assert!(matches!(chat.mapping(&row.to_string_lossy()), Resolution::Mapped { .. }));
    std::fs::remove_file(home.config().join("hosts.toml")).unwrap();
    assert_eq!(chat.status(), Status::Running);
    assert!(chat.service().is_some());
    chat.request_stop();
    assert!(chat.wait_retired(LONG));
    assert_eq!(seen.opened(), both, "a running chat read a chat file again");
}

/// The log line is `Status`'s display: a refused `chat.toml` and a refused `chat-secrets.toml` say which file.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_names_the_file_it_came_from() {
    let home = Home::new(Some(SELF));
    let row = home.folder("a");
    let (called, registry) = (Arc::new(AtomicBool::new(false)), Registry::new());
    let status = |home: &Home| begin(home, &[fact(&row, false)], &Observed::new(), &registry, never(&called)).status().to_string();

    home.secrets();
    home.write(CONFIG_FILE, "workspace = \"T0AAAA\"\nchannels = []\nbot_token = \"T0AAAA\"\n");
    let toml = status(&home);
    home.chat_toml(&[row.to_string_lossy().into_owned()]);
    home.write(SECRETS_FILE, "workspace = \"T0AAAA\"\nbot_token = \"xoxb-1111-2222-aaaa\"\nchannels = []\n");
    let secrets = status(&home);
    assert!(toml.contains(CONFIG_FILE) && !toml.contains(SECRETS_FILE), "{toml}");
    assert!(secrets.contains(SECRETS_FILE) && !secrets.contains(CONFIG_FILE), "{secrets}");
    // A key the file must not hold is named; no value from either file is.
    for (shown, key) in [(&toml, "bot_token"), (&secrets, "channels")] {
        assert!(shown.contains(&format!("\"{key}\"")) && !shown.contains("xoxb") && !shown.contains("T0AAAA"), "{shown}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hub_with_missing_or_refused_files_is_off_and_writes_nothing() {
    let home = Home::new(Some(SELF));
    let row = home.folder("a");
    home.secrets();
    let before = |h: &Home| std::fs::read_dir(h.config()).unwrap().count();
    let (called, registry) = (Arc::new(AtomicBool::new(false)), Registry::new());

    // No chat.toml: off, only that file looked for, nothing created, no lock folder, no start.
    let seen = Observed::new();
    let chat = begin(&home, &[fact(&row, false)], &seen, &registry, never(&called));
    assert_eq!(chat.status(), Status::Refused(ConfigError::Missing));
    assert!(chat.status().to_string().contains("not configured"));
    assert_eq!(seen.opened(), vec![home.config().join(CONFIG_FILE)]);
    assert_eq!(before(&home), 2, "hosts.toml and the secrets, and nothing written");
    assert!(!home.comm().exists(), "chat made the comm folder before it had a configuration");

    // A malformed chat.toml: no delivery, and the secrets stay unopened.
    home.write(CONFIG_FILE, "workspace = [[[");
    let seen = Observed::new();
    let chat = begin(&home, &[fact(&row, false)], &seen, &registry, never(&called));
    assert!(matches!(chat.status(), Status::Refused(ConfigError::Malformed(_))));
    assert_eq!(seen.opened(), vec![home.config().join(CONFIG_FILE)]);
    assert!(matches!(chat.mapping(&row.to_string_lossy()), Resolution::Invalid(ConfigError::Malformed(_))));

    // Good chat.toml, no secrets: both looked for, no start.
    home.chat_toml(&[row.to_string_lossy().into_owned()]);
    std::fs::remove_file(home.config().join(SECRETS_FILE)).unwrap();
    let seen = Observed::new();
    let chat = begin(&home, &[fact(&row, false)], &seen, &registry, never(&called));
    assert_eq!(chat.status(), Status::Secrets(ConfigError::Missing));
    assert_eq!(seen.opened().len(), 2);
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(registry.len(), 0, "nothing started, so nothing to retire");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_hub_in_a_shared_home_answers_not_hub_where_a_hub_without_the_row_refuses() {
    // One shared home: a chat.toml that maps a folder this daemon holds no row for.
    let shared = |hub: &str| {
        let home = Home::new(Some(hub));
        let row = home.configured();
        (home, row)
    };
    let (home, row) = shared("elsewhere");
    let non_hub = begin(&home, &[], &Observed::new(), &Registry::new(), refuses("unused"));
    assert_eq!(non_hub.mapping(&row.to_string_lossy()), Resolution::NotHub);
    assert!(Resolution::NotHub.explain().contains("not the configured hub"));
    // The same home on the hub, with no such row, is `UnknownFolder`: the hub decision came before the load.
    let (home, row) = shared(SELF);
    let hub = begin(&home, &[], &Observed::new(), &Registry::new(), refuses("unused"));
    assert_eq!(hub.status(), Status::Refused(ConfigError::UnknownFolder(1)));
    assert_eq!(hub.mapping(&row.to_string_lossy()), Resolution::Invalid(ConfigError::UnknownFolder(1)));
    // With its row, the hub maps the folder.
    let hub = begin(&home, &[fact(&row, false)], &Observed::new(), &Registry::new(), refuses("held"));
    assert!(matches!(hub.mapping(&row.to_string_lossy()), Resolution::Mapped { .. }));
}

#[test]
fn mappings_are_admitted_against_the_rows_canonical_folders() {
    let home = Home::new(None);
    let row = home.folder("a");
    let _other = home.folder("b");
    let rows = [fact(&row, false)];
    let text = row.to_string_lossy().into_owned();
    let want = Some(text.clone());
    let of = |spelling: &str| folder_of(&rows, spelling).map(|f| f.canonical);

    // Every spelling of the row's folder is the one canonical string: a trailing slash, `.` and `..`.
    assert_eq!(of(&text), want);
    assert_eq!(of(&format!("{text}/")), want);
    assert_eq!(of(&format!("{text}/./")), want);
    assert_eq!(of(&format!("{text}/../a")), want);
    #[cfg(unix)]
    {
        let link = home.dir.path().join("link");
        std::os::unix::fs::symlink(&row, &link).unwrap();
        assert_eq!(of(&link.to_string_lossy()), want, "a symlink to the row");
    }
    #[cfg(windows)]
    {
        // The drive letter's case and the separators are the system's, not the spelling's.
        let mut odd = text.replace('\\', "/");
        odd[..1].make_ascii_lowercase();
        assert_eq!(of(&odd), want, "{odd}");
    }
    // A relative spelling, a folder no row holds and one that does not exist are unknown.
    assert_eq!(of("a"), None);
    assert_eq!(of("../a"), None);
    assert_eq!(of(&home.folder("b").to_string_lossy()), None);
    assert_eq!(of(&format!("{text}/missing")), None);
    // The inert anchor is read through the same fact, and a row of the root that runs makes it not inert.
    assert!(folder_of(&[fact(&row, true)], &text).unwrap().inert);
    assert!(!folder_of(&[fact(&row, true), fact(&row, false)], &text).unwrap().inert);

    // Through the load: two spellings of one folder are one folder (M1's alias rule against real paths).
    home.chat_toml(&[text.clone(), format!("{text}/../a")]);
    let read = |p: &Path| config::read_file(p);
    let loaded = Config::load(&home.config(), &read, &|f| folder_of(&rows, f));
    assert_eq!(loaded.unwrap_err(), ConfigError::FolderRepeated(2));
    home.chat_toml(&[text.clone()]);
    let loaded = Config::load(&home.config(), &read, &|f| folder_of(&[fact(&row, true)], f));
    assert_eq!(loaded.unwrap_err(), ConfigError::InertAnchor(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycles_retirement_stops_the_running_service_within_its_bound() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (registry, listener) = (Registry::new(), Arc::new(Fake::default()));
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, gives(&listener));
    assert_eq!(settled(&chat).await, Status::Running);
    assert!(chat.service().is_some());
    assert_eq!(registry.len(), 1);
    assert!(!chat.wait_retired(Duration::ZERO), "running is not retired");

    chat.close_admission();
    let done = retire(&registry, LONG);
    assert!(done.incomplete.is_empty(), "{done:?}");
    assert_eq!(listener.stops.load(Ordering::SeqCst), 1);
    assert_eq!(chat.status(), Status::Stopped);
    assert!(chat.service().is_none(), "no service is handed out once the stop began");
    assert!(chat.wait_retired(Duration::ZERO));
}

/// Yield until the listener's stop has begun, failing at a 10 s bound. A retirement reports at its bound whether or
/// not its thread has reached `request_stop` yet (lifecycle's bound covers every step), so what the stop sets is read
/// after this, never right after `retire` returns.
async fn stop_begun(listener: &Fake) {
    let begun = async {
        while listener.stops.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), begun).await.expect("the retirement never asked the service to stop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_that_does_not_finish_is_reported_incomplete_and_final() {
    // Twice: as it usually runs, and with the stop asked only after the report, the order a loaded host gives (the
    // retirement's thread had not reached the chat within the bound), forced by holding the phase lock across it.
    for late in [false, true] {
        let home = Home::new(Some(SELF));
        let row = home.configured();
        let (registry, listener) = (Registry::new(), Arc::new(Fake { hang: true, ..Fake::default() }));
        let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, gives(&listener));
        assert_eq!(settled(&chat).await, Status::Running);

        let held = late.then(|| chat.shared.lock());
        let done = retire(&registry, Duration::from_millis(50));
        drop(held);
        assert_eq!(done.incomplete, vec!["chat"], "late: {late}");
        stop_begun(&listener).await;
        assert_eq!(chat.status(), Status::Stopping, "late: {late}");
        assert!(chat.service().is_none());
        // A second ask neither stops again nor reads as retired.
        chat.request_stop();
        assert!(!chat.wait_retired(Duration::ZERO));
        assert_eq!(listener.stops.load(Ordering::SeqCst), 1, "late: {late}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_under_way_when_retirement_comes_is_ended_with_it() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let registry = Registry::new();
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, pending());
    assert_eq!(chat.status(), Status::Starting);
    let done = retire(&registry, LONG);
    assert!(done.incomplete.is_empty(), "{done:?}");
    assert_eq!(chat.status(), Status::Stopped);
    assert!(chat.service().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn retirement_that_began_first_refuses_the_start() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (registry, called) = (Registry::new(), Arc::new(AtomicBool::new(false)));
    retire(&registry, LONG);
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, never(&called));
    assert_eq!(chat.status(), Status::Stopped);
    assert!(chat.service().is_none());
    assert!(!called.load(Ordering::SeqCst), "a registrant after retirement began must not start");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_fails_leaves_the_chat_off_with_its_code_and_a_private_lock_folder_is_given() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let registry = Registry::new();
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, refuses("down"));
    assert_eq!(settled(&chat).await, Status::Failed("down"));
    assert!(chat.status().to_string().contains("down"));
    assert!(chat.service().is_none());
    assert!(chat.wait_retired(Duration::ZERO));
    assert!(retire(&registry, LONG).incomplete.is_empty());

    // The lock folder the starter gets is the comm folder's `chat`, made private, and not the daemon's state folder's.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let record = seen.clone();
    let start: Starter<Fake> = Box::new(move |_, _, dir| {
        *record.lock().unwrap() = Some(dir.to_path_buf());
        Box::pin(async move { Err("seen") })
    });
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &Registry::new(), start);
    settled(&chat).await;
    let dir = seen.lock().unwrap().clone().unwrap();
    assert_eq!(dir, home.comm().join("chat"));
    assert!(sot_platform::host::state_dir::is_private_dir(&dir));
    assert!(!sot_platform::host::state_dir::daemon_state_dir_in(&home.env).join("chat").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_chat_lets_its_listener_go_and_a_start_after_a_stop_is_a_new_chat() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (registry, first) = (Registry::new(), Arc::new(Fake::default()));
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, gives(&first));
    assert_eq!(settled(&chat).await, Status::Running);
    // Dropped without a stop: nothing here holds the listener, so its own drop ends it (chat guarantee 22).
    drop((chat, registry));
    let began = std::time::Instant::now();
    while Arc::strong_count(&first) > 1 {
        let seen = Arc::strong_count(&first);
        assert!(began.elapsed() < Duration::from_secs(10), "the dropped chat still holds its listener: strong count {seen}");
        tokio::task::yield_now().await;
    }
    assert_eq!(first.stops.load(Ordering::SeqCst), 0);

    // After a stop, a start is a second `begin`, with a registry of its own.
    let (registry, second) = (Registry::new(), Arc::new(Fake::default()));
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, gives(&second));
    assert_eq!(settled(&chat).await, Status::Running);
    assert!(retire(&registry, LONG).incomplete.is_empty());
    let (registry, third) = (Registry::new(), Arc::new(Fake::default()));
    let again = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, gives(&third));
    assert_eq!(settled(&again).await, Status::Running);
    assert_eq!(chat.status(), Status::Stopped, "the first chat stays stopped");
    assert_eq!(second.stops.load(Ordering::SeqCst), 1);
}

/// A starter that holds `guard` while it waits for ever.
fn holding(guard: Arc<()>) -> Starter<Fake> {
    Box::new(move |_, _, _| {
        let guard = guard.clone();
        Box::pin(async move {
            let _held = guard;
            std::future::pending().await
        })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn retiring_a_start_under_way_drops_its_future() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (registry, probe) = (Registry::new(), Arc::new(()));
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, holding(probe.clone()));
    // The start task holds the probe from its first poll until it is dropped by the abort.
    while Arc::strong_count(&probe) == 1 {
        tokio::task::yield_now().await;
    }
    assert!(retire(&registry, LONG).incomplete.is_empty());
    let dropped = async {
        while Arc::strong_count(&probe) > 1 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), dropped)
        .await
        .expect("a start under way was retired but its future was not dropped: the abort is missing");
    assert_eq!(chat.status(), Status::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_finishes_as_retirement_arrives_is_stopped_at_once() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (registry, listener) = (Arc::new(Registry::new()), Arc::new(Fake::default()));
    let (reg, made) = (registry.clone(), listener.clone());
    // The retirement runs inside the start, just before it returns its service.
    let start: Starter<Fake> = Box::new(move |_, _, _| {
        let (reg, made) = (reg.clone(), made.clone());
        Box::pin(async move {
            assert!(retire(&reg, LONG).incomplete.is_empty());
            Ok(made)
        })
    });
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, start);
    let stopped = async {
        while listener.stops.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), stopped)
        .await
        .expect("a service made as retirement arrived was never stopped: the spawned stop is missing");
    assert_eq!(chat.status(), Status::Stopped);
    assert!(chat.service().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lock_folder_that_cannot_be_made_leaves_the_chat_off_unregistered() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    // A file where the folder belongs.
    std::fs::create_dir_all(home.comm()).unwrap();
    std::fs::write(home.comm().join("chat"), "not a folder").unwrap();
    let (registry, called) = (Registry::new(), Arc::new(AtomicBool::new(false)));
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, never(&called));
    assert_eq!(chat.status(), Status::Failed("lock"));
    assert_eq!(registry.len(), 0);
    assert!(!called.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retirement_reaching_request_stop_between_register_and_starting_finds_the_start() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let registry = Arc::new(Registry::new());
    let retiring = Arc::new(std::sync::Mutex::new(None));
    // Right after the chat is registered, a retirement starts and is given time to reach `request_stop`: with the
    // phase lock held by `begin` it waits there for `Starting`; with the lock taken after the registration it has
    // already finished against a chat that was off. (One sleep: a slow host can only miss the second case.)
    let (reg, slot) = (registry.clone(), retiring.clone());
    registry.on_register(Box::new(move || {
        let reg = reg.clone();
        *slot.lock().unwrap() = Some(std::thread::spawn(move || retire(&reg, LONG)));
        std::thread::sleep(Duration::from_millis(50));
    }));
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, pending());
    let done = retiring.lock().unwrap().take().unwrap().join().unwrap();
    assert!(done.incomplete.is_empty(), "{done:?}");
    assert_eq!(chat.status(), Status::Stopped, "a start began after its retirement was reported complete");
}

// ---- P18: one listener per comm folder ----

/// How often the tests' chats retry and ask the hub fact.
const FAST: Duration = Duration::from_millis(10);

/// Yield and sleep until `ready`, failing at a 10 s bound.
async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let waiting = async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), waiting).await.unwrap_or_else(|_| panic!("never: {what}"));
}

/// A fake Slack that names the workspace and refuses every socket ticket, so a service runs with no live socket.
async fn slack() -> sot_chat::fake_slack::FakeSlack {
    use sot_chat::fake_slack::{FakeSlack, Identity, Reply};
    FakeSlack::start(Identity::Trusted, |seen, _| {
        if seen.path().ends_with("/auth.test") {
            Reply::json(&serde_json::json!({"ok": true, "team_id": "T0AAAA", "user_id": "U0BOT", "bot_id": "B0BOT"}))
        } else {
            Reply::json(&serde_json::json!({"ok": false, "error": "invalid_auth"}))
        }
    })
    .await
}

fn identity_reads(fake: &sot_chat::fake_slack::FakeSlack) -> usize {
    fake.seen().iter().filter(|s| s.path().ends_with("/auth.test")).count()
}

/// The product's service against `fake`, counting each try.
fn counting(fake: &sot_chat::fake_slack::FakeSlack, tries: &Arc<AtomicUsize>) -> Starter<Service> {
    let (http, base, tries) = (fake.http(), fake.url("slack.com", "/api/"), tries.clone());
    Box::new(move |config, credentials, dir| {
        tries.fetch_add(1, Ordering::SeqCst);
        let (http, base) = (http.clone(), base.clone());
        Box::pin(async move { Service::start_at(config, credentials, http, dir, base).await.map(Arc::new).map_err(|e| e.code()) })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn two_daemons_on_one_comm_folder_run_one_listener() {
    // Two daemons, each with a state folder of its own, one comm folder: the second is held, calls Slack for nothing,
    // and listens once the first lets go.
    let fake = slack().await;
    let one = Home::new(Some(SELF));
    let mut two = Home::new(Some(SELF));
    two.env = two.env.with("SOT_COMM_HOME", one.comm());
    let (rows, tries) = ([one.configured(), two.configured()], [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))]);
    let registries = [Registry::new(), Registry::new()];
    let start = |home: &Home, i: usize| {
        let read = |p: &Path| config::read_file(p);
        Chat::<Service>::begin(&home.env, &[fact(&rows[i], false)], &read, &registries[i], counting(&fake, &tries[i]), FAST)
    };
    let first = start(&one, 0);
    assert_eq!(settled(&first).await, Status::Running);
    let second = start(&two, 1);
    until("the second daemon's retry or its end", || tries[1].load(Ordering::SeqCst) >= 2 || second.status() != Status::Starting).await;
    assert_eq!(second.status(), Status::Starting, "held, not failed");
    assert_eq!(identity_reads(&fake), 1, "a second listener read the bot's identity");

    first.request_stop();
    assert!(first.wait_retired(LONG));
    until("the second daemon takes the lock", || second.status() == Status::Running).await;
    assert_eq!(identity_reads(&fake), 2);
    assert!(retire(&registries[1], LONG).incomplete.is_empty());
    fake.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_start_starts_once_the_holder_lets_go() {
    let fake = slack().await;
    let home = Home::new(Some(SELF));
    let folder = home.configured();
    let rows = Workspaces::new();
    rows.insert(row("alpha", &folder));
    // Another holder of the workspace's lock, as a second computer's daemon would be.
    let lock = sot_platform::host::lock_supervisor(&crate::comm::home::chat_dir(&home.env).unwrap().join("chat-T0AAAA.lock")).unwrap();
    let (registry, tries) = (Registry::new(), Arc::new(AtomicUsize::new(0)));
    let chat = Chat::boot(&home.env, &rows, &|p| config::read_file(p), &registry, counting(&fake, &tries), FAST, sot_chat::delivery::Timing::PRODUCT);
    until("a retry or the start's end", || tries.load(Ordering::SeqCst) >= 3 || chat.status() != Status::Starting).await;
    assert_eq!(chat.status(), Status::Starting, "a held start is not final, and stays starting");
    assert!(!chat.hub().running());
    drop(lock);
    until("the owner starts when the lock comes", || chat.status() == Status::Running && chat.hub().running()).await;
    assert!(retire(&registry, LONG).incomplete.is_empty());
    fake.shutdown().await;

    // A held start ends with its retirement, and with the topology naming another computer the hub.
    let (registry, held) = (Registry::new(), Home::new(Some(SELF)));
    let row = held.configured();
    let retired = Chat::<Fake>::begin(&held.env, &[fact(&row, false)], &|p| config::read_file(p), &registry, refuses("held"), FAST);
    tokio::time::sleep(5 * FAST).await;
    assert_eq!(retired.status(), Status::Starting);
    assert!(retire(&registry, LONG).incomplete.is_empty());
    assert_eq!(retired.status(), Status::Stopped);
    let moved = Chat::<Fake>::begin(&held.env, &[fact(&row, false)], &|p| config::read_file(p), &Registry::new(), refuses("held"), FAST);
    held.list("elsewhere");
    until("the held start gives up", || moved.status() == Status::NotHub).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_start_survives_a_topology_that_does_not_parse() {
    // A hosts.toml being edited (unparsable, or briefly gone) is not another computer named the hub: the held start
    // keeps trying and starts when the lock comes.
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (listener, free, tries) = (Arc::new(Fake::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let (made, open, count) = (listener.clone(), free.clone(), tries.clone());
    let start: Starter<Fake> = Box::new(move |_, _, _| {
        count.fetch_add(1, Ordering::SeqCst);
        let (made, open) = (made.clone(), open.load(Ordering::SeqCst));
        Box::pin(async move { if open { Ok(made) } else { Err("held") } })
    });
    let chat = Chat::begin(&home.env, &[fact(&row, false)], &|p| config::read_file(p), &Registry::new(), start, FAST);
    for what in ["unparsable", "missing"] {
        match what {
            "unparsable" => home.write("hosts.toml", "hub = \n[[["),
            _ => std::fs::remove_file(home.config().join("hosts.toml")).unwrap(),
        }
        let seen = tries.load(Ordering::SeqCst);
        until("more tries or the start's end", || tries.load(Ordering::SeqCst) >= seen + 3 || chat.status() != Status::Starting).await;
        assert_eq!(chat.status(), Status::Starting, "{what}");
    }
    home.list(SELF);
    free.store(true, Ordering::SeqCst);
    until("the start", || chat.status() == Status::Running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_whose_topology_names_another_hub_steps_down_and_a_broken_topology_changes_nothing() {
    // The predicate: only a topology that parses and a name that is not empty.
    let home = Home::new(Some("elsewhere"));
    let elsewhere = sot_topology::load_in(&home.env);
    assert!(names_other_hub(&elsewhere, SELF));
    assert!(!names_other_hub(&elsewhere, "elsewhere"), "this computer is the hub");
    assert!(!names_other_hub(&elsewhere, ""), "an empty self-name is not another computer naming the hub");
    assert!(!names_other_hub(&Ok(None), SELF) && !names_other_hub(&Err("unreadable".into()), SELF));
    assert!(names_another_hub(&home.env));

    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (registry, listener) = (Registry::new(), Arc::new(Fake::default()));
    let chat = Chat::begin(&home.env, &[fact(&row, false)], &|p| config::read_file(p), &registry, gives(&listener), FAST);
    assert_eq!(settled(&chat).await, Status::Running);
    // A topology that does not parse, one that is gone, and one naming this computer: the chat runs on through many asks.
    for what in ["unparsable", "missing", "this computer"] {
        match what {
            "unparsable" => home.write("hosts.toml", "hub = \n[[["),
            "missing" => std::fs::remove_file(home.config().join("hosts.toml")).unwrap(),
            _ => home.list(SELF),
        }
        tokio::time::sleep(15 * FAST).await;
        assert_eq!(chat.status(), Status::Running, "{what}");
        assert_eq!(listener.stops.load(Ordering::SeqCst), 0, "{what}");
    }
    // Another computer named the hub: the service stops, which frees its lock, and the status says why.
    home.list("elsewhere");
    until("the step-down", || chat.status() == Status::NotHub).await;
    assert_eq!(listener.stops.load(Ordering::SeqCst), 1);
    assert!(chat.service().is_none());
    assert!(retire(&registry, LONG).incomplete.is_empty());
    assert_eq!(listener.stops.load(Ordering::SeqCst), 1, "a retirement after the step-down stops nothing again");
}

// ---- M3: the adapter every chat action passes ----

use sot_protocol::ops::ChatChannel;

pub(crate) fn row(label: &str, folder: &Path) -> Workspace {
    Workspace::from_label(label, folder.to_path_buf(), false, "none".into(), String::new(), String::new())
}

fn target(row: &str, workspace: &str, channel: &str) -> ChatTarget {
    ChatTarget { row: row.into(), workspace: workspace.into(), channel: ChatChannel::try_from(channel.to_string()).unwrap() }
}

/// A hub with a running service, rows `alpha` (folder a, mapped to C0AAAA) and `beta` (folder b, C0BBBB), and a folder
/// c no mapping names. The service is a listener that holds no Slack: a refusal is proved by the adapter handing out
/// no service, the only way to a call (`Chat::service` is private), since the backend's tests do not use the fake Slack yet.
struct World {
    /// Keeps the temporary folders alive.
    _home: Home,
    rows: Workspaces,
    chat: Arc<Chat<Fake>>,
    alpha: String,
    folders: [PathBuf; 3],
}

async fn world() -> World {
    let home = Home::new(Some(SELF));
    let folders = [home.folder("a"), home.folder("b"), home.folder("c")];
    home.delivering_toml(&[folders[0].to_string_lossy().into_owned(), folders[1].to_string_lossy().into_owned()]);
    home.secrets();
    let rows = Workspaces::new();
    let alpha = rows.insert(row("alpha", &folders[0])).workspace_id.clone();
    rows.insert(row("beta", &folders[1]));
    let chat = begin(&home, &row_facts(&rows), &Observed::new(), &Registry::new(), gives(&Arc::new(Fake::default())));
    assert_eq!(settled(&chat).await, Status::Running);
    World { _home: home, rows, chat, alpha, folders }
}

fn admitted(w: &World, id: &str, workspace: &str, channel: &str) -> Result<(), ChatCode> {
    w.chat.admit(&w.rows, &target(id, workspace, channel)).map(|_| ())
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_row_or_mapping_cannot_post_or_fetch() {
    let w = world().await;
    let id = w.alpha.clone();
    assert_eq!(admitted(&w, &id, "T0AAAA", "C0AAAA"), Ok(()));

    // A removed row, an id that never existed, an empty id and a slug used as an id: no service.
    for what in ["ws-nothing-0", "", "alpha"] {
        assert_eq!(admitted(&w, what, "T0AAAA", "C0AAAA"), Err(ChatCode::StaleRow), "{what:?}");
    }
    // A watch admitted while the row was current: once the row goes, its next snapshot is refused, and an unwatch is
    // still admitted so the watch can be dropped.
    assert!(w.chat.admit(&w.rows, &target(&id, "T0AAAA", "C0AAAA")).is_ok());
    w.rows.remove_by_id(&id).unwrap();
    assert_eq!(admitted(&w, &id, "T0AAAA", "C0AAAA"), Err(ChatCode::StaleRow));
    assert_eq!(w.chat.admit_unwatch(), Ok(()));

    // Replaced: a new row in the folder under a new id does not inherit a request that named the old id, and a request
    // naming the new id works.
    let new = w.rows.insert(row("alpha", &w.folders[0])).workspace_id.clone();
    assert_ne!(new, id);
    assert_eq!(admitted(&w, &id, "T0AAAA", "C0AAAA"), Err(ChatCode::StaleRow));
    assert_eq!(admitted(&w, &new, "T0AAAA", "C0AAAA"), Ok(()));

    // A pair the mapping does not hold: another channel, another workspace, a channel the file does not list.
    for (workspace, channel) in [("T0AAAA", "C0BBBB"), ("T0ZZZZ", "C0AAAA"), ("T0AAAA", "C0ZZZZ")] {
        assert_eq!(admitted(&w, &new, workspace, channel), Err(ChatCode::StaleMapping), "{workspace} {channel}");
    }
    // The same id re-rooted (a same-slug insert keeps the id): onto folder b it maps to C0BBBB, not what was expected;
    // onto folder c it maps to nothing; and the row that went away with its folder is gone.
    w.rows.insert(row("alpha", &w.folders[1]));
    assert_eq!(admitted(&w, &new, "T0AAAA", "C0AAAA"), Err(ChatCode::StaleMapping));
    assert_eq!(admitted(&w, &new, "T0AAAA", "C0BBBB"), Ok(()));
    w.rows.insert(row("alpha", &w.folders[2]));
    assert_eq!(admitted(&w, &new, "T0AAAA", "C0AAAA"), Err(ChatCode::Unmapped));
    std::fs::remove_dir_all(&w.folders[2]).unwrap();
    assert_eq!(admitted(&w, &new, "T0AAAA", "C0AAAA"), Err(ChatCode::StaleRow));

    // The inert anchor never acts, though its folder is still mapped.
    w.rows.insert(row("alpha", &w.folders[0]));
    assert_eq!(admitted(&w, &new, "T0AAAA", "C0AAAA"), Ok(()));
    w.rows.set_default(&new);
    assert_eq!(admitted(&w, &new, "T0AAAA", "C0AAAA"), Err(ChatCode::Unmapped));
    // With a default set, an empty id is still no row: the registry's lookup answers it with the default, and only
    // the exact-id compare in `current_row` refuses it.
    assert_eq!(admitted(&w, "", "T0AAAA", "C0AAAA"), Err(ChatCode::StaleRow));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_action_is_admitted_unless_the_service_runs() {
    let rows = Workspaces::new();
    let t = |id: &str| target(id, "T0AAAA", "C0AAAA");
    let unavailable = |chat: &Chat<Fake>, id: &str| chat.admit(&rows, &t(id)).err() == Some(ChatCode::Unavailable) && chat.admit_unwatch() == Err(ChatCode::Unavailable);

    // Not the hub, a hub with no chat.toml, one with no secrets, and one whose start failed.
    let other = Home::new(Some("elsewhere"));
    let folder = other.configured();
    assert!(unavailable(&begin(&other, &[fact(&folder, false)], &Observed::new(), &Registry::new(), refuses("x")), "r"));
    let bare = Home::new(Some(SELF));
    assert!(unavailable(&begin(&bare, &[], &Observed::new(), &Registry::new(), refuses("x")), "r"));
    let no_secrets = Home::new(Some(SELF));
    let folder = no_secrets.configured();
    std::fs::remove_file(no_secrets.config().join(SECRETS_FILE)).unwrap();
    assert!(unavailable(&begin(&no_secrets, &[fact(&folder, false)], &Observed::new(), &Registry::new(), refuses("x")), "r"));
    let failing = Home::new(Some(SELF));
    let folder = failing.configured();
    let chat = begin(&failing, &[fact(&folder, false)], &Observed::new(), &Registry::new(), refuses("down"));
    assert_eq!(settled(&chat).await, Status::Failed("down"));
    assert!(unavailable(&chat, "r"));

    // Starting, and stopped after running.
    let starting = Home::new(Some(SELF));
    let folder = starting.configured();
    let chat = begin(&starting, &[fact(&folder, false)], &Observed::new(), &Registry::new(), pending());
    assert_eq!(chat.status(), Status::Starting);
    assert!(unavailable(&chat, "r"));
    let w = world().await;
    let t = target(&w.alpha, "T0AAAA", "C0AAAA");
    assert!(w.chat.admit(&w.rows, &t).is_ok());
    w.chat.request_stop();
    assert!(w.chat.wait_retired(LONG));
    assert_eq!(w.chat.admit(&w.rows, &t).err(), Some(ChatCode::Unavailable));
    assert_eq!(w.chat.admit_unwatch(), Err(ChatCode::Unavailable));
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_joins_the_mapping_with_the_row_and_the_service_status() {
    let w = world().await;
    let mapped = |delivers| Ok((Resolution::Mapped { workspace: "T0AAAA".into(), channel: "C0AAAA".into(), mail_after: Some(sot_protocol::chat::Ts::parse("1.000000").unwrap()) }, delivers));
    assert_eq!(w.chat.resolve(&w.rows, &w.alpha), mapped(true));
    let gamma = w.rows.insert(row("gamma", &w.folders[2])).workspace_id.clone();
    assert_eq!(w.chat.resolve(&w.rows, &gamma), Ok((Resolution::Unmapped, false)));
    assert_eq!(w.chat.resolve(&w.rows, ""), Err(ChatCode::StaleRow));
    assert_eq!(w.chat.resolve(&w.rows, "alpha"), Err(ChatCode::StaleRow));
    w.rows.set_default(&w.alpha);
    assert_eq!(w.chat.resolve(&w.rows, &w.alpha), Ok((Resolution::Unmapped, false)));
    w.rows.set_default(&gamma);
    // Once the service is stopped the file's mapping still shows, and nothing delivers.
    w.chat.request_stop();
    assert!(w.chat.wait_retired(LONG));
    assert_eq!(w.chat.resolve(&w.rows, &w.alpha), mapped(false));
    w.rows.remove_by_id(&w.alpha);
    assert_eq!(w.chat.resolve(&w.rows, &w.alpha), Err(ChatCode::StaleRow));

    // Starting, a start that failed, and missing secrets show the mapping and do not deliver; not the hub and a
    // refused chat.toml say so.
    let starting = Home::new(Some(SELF));
    let folder = starting.configured();
    starting.delivering_toml(&[folder.to_string_lossy().into_owned()]);
    let rows = Workspaces::new();
    let id = rows.insert(row("alpha", &folder)).workspace_id.clone();
    let chat = begin(&starting, &row_facts(&rows), &Observed::new(), &Registry::new(), pending());
    assert_eq!(chat.resolve(&rows, &id), mapped(false));
    let failed = begin(&starting, &row_facts(&rows), &Observed::new(), &Registry::new(), refuses("down"));
    assert_eq!(settled(&failed).await, Status::Failed("down"));
    assert_eq!(failed.resolve(&rows, &id), mapped(false));
    std::fs::remove_file(starting.config().join(SECRETS_FILE)).unwrap();
    let bare = begin(&starting, &row_facts(&rows), &Observed::new(), &Registry::new(), refuses("x"));
    assert_eq!(bare.status(), Status::Secrets(ConfigError::Missing));
    assert_eq!(bare.resolve(&rows, &id), mapped(false));
    starting.write(CONFIG_FILE, "workspace = [[[");
    let refused = begin(&starting, &row_facts(&rows), &Observed::new(), &Registry::new(), refuses("x"));
    assert!(matches!(refused.resolve(&rows, &id), Ok((Resolution::Invalid(ConfigError::Malformed(_)), false))));
    let non_hub = Home::new(Some("elsewhere"));
    let chat = begin(&non_hub, &[], &Observed::new(), &Registry::new(), refuses("x"));
    assert_eq!(chat.resolve(&rows, &id), Ok((Resolution::NotHub, false)));
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_asks_the_hub_fact_first_and_the_row_before_the_file() {
    let rows = Workspaces::new();
    let folders = Home::new(None);
    let id = rows.insert(row("alpha", &folders.folder("a"))).workspace_id.clone();
    // A non-hub answers `NotHub` even for an id it does not know.
    let other = Home::new(Some("elsewhere"));
    let chat = begin(&other, &[], &Observed::new(), &Registry::new(), refuses("x"));
    assert_eq!(chat.resolve(&rows, "ws-nothing-0"), Ok((Resolution::NotHub, false)));
    // A hub with no chat.toml: `Invalid(Missing)` for a row it has, `StaleRow` for one it has not.
    let bare = Home::new(Some(SELF));
    let chat = begin(&bare, &[], &Observed::new(), &Registry::new(), refuses("x"));
    assert_eq!(chat.resolve(&rows, &id), Ok((Resolution::Invalid(ConfigError::Missing), false)));
    assert_eq!(chat.resolve(&rows, "ws-nothing-0"), Err(ChatCode::StaleRow));
}

/// The seam the chat ops' tests use: the fake Slack from `sot-chat` answers one scripted reply to an `Http` built by
/// `Http::for_test`, and counts the call.
#[tokio::test(flavor = "multi_thread")]
async fn the_exported_fake_slack_answers_one_scripted_reply() {
    use sot_chat::fake_slack::{FakeSlack, Identity, Reply};
    use sot_chat::http::Request;
    let fake = FakeSlack::start(Identity::Trusted, |_, _| Reply::ok(b"scripted")).await;
    let reply = fake
        .http()
        .send(Request::get(fake.url("slack.com", "/api/x"), LONG))
        .await
        .expect("the fake answers");
    assert_eq!((reply.status(), reply.body()), (200, &b"scripted"[..]));
    assert_eq!(fake.seen().len(), 1);
    fake.shutdown().await;
}
