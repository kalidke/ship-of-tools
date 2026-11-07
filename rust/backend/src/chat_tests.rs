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
            home.write("hosts.toml", &format!("hub = \"{hub}\"\n[host.{hub}]\ndaemon = true\n"));
        }
        home
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
        let _ = (chat.status().to_string(), chat.resolve(&row.to_string_lossy()), chat.service().is_none());
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
    home.write("hosts.toml", "hub = \"elsewhere\"\n[host.elsewhere]\ndaemon = true\n");
    let _ = (chat.status(), chat.service().is_some(), chat.resolve(&row.to_string_lossy()));
    std::fs::remove_file(home.config().join("hosts.toml")).unwrap();
    let _ = (chat.status().to_string(), chat.service().is_some(), chat.resolve("/x"));
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
    assert!(matches!(chat.resolve(&row.to_string_lossy()), Resolution::Invalid(ConfigError::Malformed(_))));

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
    assert_eq!(non_hub.resolve(&row.to_string_lossy()), Resolution::NotHub);
    assert!(Resolution::NotHub.explain().contains("not the configured hub"));
    // The same home on the hub, with no such row, is `UnknownFolder`: the hub decision came before the load.
    let (home, row) = shared(SELF);
    let hub = begin(&home, &[], &Observed::new(), &Registry::new(), refuses("unused"));
    assert_eq!(hub.status(), Status::Refused(ConfigError::UnknownFolder(1)));
    assert_eq!(hub.resolve(&row.to_string_lossy()), Resolution::Invalid(ConfigError::UnknownFolder(1)));
    // With its row, the hub maps the folder.
    let hub = begin(&home, &[fact(&row, false)], &Observed::new(), &Registry::new(), refuses("held"));
    assert!(matches!(hub.resolve(&row.to_string_lossy()), Resolution::Mapped { .. }));
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
