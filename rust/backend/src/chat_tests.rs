use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sot_chat::config::{CONFIG_FILE, SECRETS_FILE};

use crate::lifecycle::retire::retire;

const SELF: &str = "hub-box";
const BOT: &str = "xoxb-1111-2222-aaaa";
const APP: &str = "xapp-1-A111-2222-bbbb";
/// A bound no test relies on meeting: it only has to be long enough for a stop that finishes.
const LONG: Duration = Duration::from_secs(60);

/// Synthetic homes: a config home, a state home and the folders of rows, none of them the real ones.
struct Home {
    dir: tempfile::TempDir,
    env: Env,
}

impl Home {
    /// `hub` names the hub in hosts.toml (`None`: no hosts.toml at all).
    fn new(hub: Option<&str>) -> Home {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let env = Env::empty()
            .with("HOME", root.join("home"))
            .with("XDG_CONFIG_HOME", root.join("config"))
            .with("XDG_STATE_HOME", root.join("state"))
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
    fn list(&self, hub: &str) {
        let me = if hub == SELF { String::new() } else { format!("[host.{SELF}]\ndaemon = true\n") };
        self.write("hosts.toml", &format!("hub = \"{hub}\"\n[host.{hub}]\ndaemon = true\n{me}"));
    }

    fn config(&self) -> PathBuf {
        config::config_dir(&self.env).unwrap()
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.config().join(name), text).unwrap();
    }

    /// A folder a row could have, in its canonical spelling.
    fn folder(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join("rows").join(name);
        std::fs::create_dir_all(&p).unwrap();
        canonical(&p).unwrap()
    }

    fn chat_toml(&self, folders: &[String]) {
        let mut text = String::from("workspace = \"T0AAAA\"\nchannels = [\"C0AAAA\", \"C0BBBB\"]\nmail_after = \"1.000000\"\n");
        for (f, c) in folders.iter().zip(["C0AAAA", "C0BBBB"]) {
            text += &format!("[[mapping]]\nfolder = '{f}'\nchannel = \"{c}\"\n");
        }
        self.write(CONFIG_FILE, &text);
    }

    fn secrets(&self) {
        self.write(SECRETS_FILE, &format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT}\"\napp_token = \"{APP}\"\n"));
    }

    /// Both files, with one mapped row.
    fn configured(&self) -> PathBuf {
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
    Box::new(move |_, _, _| Box::pin(async move { Ok(listener) }))
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
    Chat::begin(&home.env, rows, &|p| seen.read(p), registry, start)
}

/// Yield until the start task has finished (no timer: it ends once its starter does).
async fn settled(chat: &Chat<Fake>) -> Status {
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
    assert!(!sot_platform::host::state_dir::daemon_state_dir_in(&home.env).join("chat").exists());

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

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_that_does_not_finish_is_reported_incomplete_and_final() {
    let home = Home::new(Some(SELF));
    let row = home.configured();
    let (registry, listener) = (Registry::new(), Arc::new(Fake { hang: true, ..Fake::default() }));
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, gives(&listener));
    assert_eq!(settled(&chat).await, Status::Running);

    let done = retire(&registry, Duration::from_millis(50));
    assert_eq!(done.incomplete, vec!["chat"]);
    assert_eq!(chat.status(), Status::Stopping);
    assert!(chat.service().is_none());
    // A second ask neither stops again nor reads as retired.
    chat.request_stop();
    assert!(!chat.wait_retired(Duration::ZERO));
    assert_eq!(listener.stops.load(Ordering::SeqCst), 1);
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
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &registry, refuses("held"));
    assert_eq!(settled(&chat).await, Status::Failed("held"));
    assert!(chat.status().to_string().contains("held"));
    assert!(chat.service().is_none());
    assert!(chat.wait_retired(Duration::ZERO));
    assert!(retire(&registry, LONG).incomplete.is_empty());

    // The lock folder the starter gets is the daemon's state folder's `chat`, made private.
    let seen = Arc::new(std::sync::Mutex::new(None));
    let record = seen.clone();
    let start: Starter<Fake> = Box::new(move |_, _, dir| {
        *record.lock().unwrap() = Some(dir.to_path_buf());
        Box::pin(async move { Err("seen") })
    });
    let chat = begin(&home, &[fact(&row, false)], &Observed::new(), &Registry::new(), start);
    settled(&chat).await;
    let dir = seen.lock().unwrap().clone().unwrap();
    assert_eq!(dir, sot_platform::host::state_dir::daemon_state_dir_in(&home.env).join("chat"));
    assert!(sot_platform::host::state_dir::is_private_dir(&dir));
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
    assert_eq!(Arc::strong_count(&first), 1);
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
    let state = sot_platform::host::state_dir::daemon_state_dir_in(&home.env);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("chat"), "not a folder").unwrap();
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

// ---- M3: the adapter every chat action passes ----

use sot_protocol::ops::ChatChannel;

fn row(label: &str, folder: &Path) -> Workspace {
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
    home.chat_toml(&[folders[0].to_string_lossy().into_owned(), folders[1].to_string_lossy().into_owned()]);
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
    // A watch admitted while the row was current: once the row goes, its next snapshot is refused and it can be dropped.
    assert!(w.chat.admit(&w.rows, &target(&id, "T0AAAA", "C0AAAA")).is_ok());
    assert_eq!(w.chat.admit_unwatch(), Ok(()));
    w.rows.remove_by_id(&id).unwrap();
    assert_eq!(admitted(&w, &id, "T0AAAA", "C0AAAA"), Err(ChatCode::StaleRow));

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
