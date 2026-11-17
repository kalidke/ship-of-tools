//! The chat's configuration: `chat.toml` (the file's workspace, its channels, the folder-to-channel mappings, an optional
//! `mail_after`) and the secrets, one file per workspace, each the workspace and its three tokens (the user token
//! optional): `chat-secrets.toml` for `chat.toml`'s workspace, `chat-secrets-<id>.toml` for any other. All sit at the top
//! of platform's config folder. Every
//! refusal is a fixed code with a fixed explanation: a value from any file is never quoted, a key is quoted only
//! when a table of these files accepts it (the caller says which file refused), and the library's own parse errors
//! (which quote the source) are never formatted. Reading the secrets is its own call, so a caller that is
//! not the hub never opens those files. The daemon's setup writes the files whole, each text one the load admits, through
//! an injected writer pair: `chat.toml` durably, the secrets durably and owner-only.

use crate::http::Secret;
use crate::normalize::ts_key;
use sot_platform::host::env::Env;
use sot_protocol::chat::Ts;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// The one canonical spelling of a folder, supplied by the daemon: `None` for a folder that does not exist on disk and
/// for a spelling that is not absolute. Chat resolves no row; whether a row has the folder is asked per action.
pub type Canon<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The one file read, so a test (and the hub's guard) sees which files are opened.
pub type Reader<'a> = &'a dyn Fn(&Path) -> io::Result<String>;

/// The most either file may hold. The cap's job is to end a stall and bound the work a planted file can cause (the
/// channel check is quadratic, and each mapping asks the filesystem); 64 KiB holds thousands of mappings.
pub(crate) const MAX_FILE: u64 = 64 * 1024;

/// The one real read: a regular file of at most [`MAX_FILE`] bytes, as UTF-8, through platform's bounded read. Not a
/// regular file (a FIFO, a device, a directory) is `InvalidInput`; a larger file or other bytes than UTF-8 is
/// `InvalidData`.
pub fn read_file(path: &Path) -> io::Result<String> {
    sot_platform::host::read_regular(path, MAX_FILE)
}

/// The most mappings a delivering file (one whose hub `mail_after` is set) may hold: the watch model's channel bound,
/// so every delivering channel can be held.
pub(crate) const MAX_DELIVERING: usize = crate::watch::MAX_CHANNELS;

/// A file's text, or why not: absent, too large or not text (`Malformed`), or unreadable.
fn text(read: Reader, path: &Path) -> Result<String, ConfigError> {
    read(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => ConfigError::Missing,
        io::ErrorKind::InvalidData => ConfigError::Malformed(None),
        _ => ConfigError::Unreadable,
    })
}

/// `$XDG_CONFIG_HOME/sot` (or the platform's own place), by platform's resolver; `None` when no base variable is set.
pub fn config_dir(env: &Env) -> Option<PathBuf> {
    sot_platform::host::state_dir::sot_config_dir_in(env)
}

pub const CONFIG_FILE: &str = "chat.toml";
/// The secrets of `chat.toml`'s workspace, in the one form an older daemon reads.
pub const SECRETS_FILE: &str = "chat-secrets.toml";
/// Any other workspace's secrets file is `chat-secrets-<id>.toml`, `<id>` a Slack id.
const NAMED_PREFIX: &str = "chat-secrets-";
const NAMED_SUFFIX: &str = ".toml";

/// The most workspaces a hub serves (wire's one copy), so the most secrets files a load opens.
use sot_protocol::ops::MAX_WORKSPACES;

/// Why a file was refused. A code, and where it is when that is a number: no key and no value from the file. A mapping's
/// ordinal counts from 1 in file order; a parse failure's line counts from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing,
    Unreadable,
    Malformed(Option<usize>),
    BadId,
    BadMailAfter(Option<usize>),
    BadToken,
    /// A key the file does not take: named when a table of the chat's files accepts it, else not named at all.
    UnknownKey(Option<&'static str>),
    SecondWorkspace,
    DuplicateChannel,
    FolderRepeated(usize),
    ChannelRepeated(usize),
    ChannelNotListed(usize),
    TooManyDelivering,
    MappingWithoutFloor(usize),
    /// A `chat-secrets-<id>.toml` holds another workspace than its `<id>`, or `<id>` is `chat.toml`'s own.
    NameNotWorkspace,
    /// A secrets file past the first `MAX_WORKSPACES` of a load: not read.
    TooManyWorkspaces,
}

impl ConfigError {
    pub fn explain(self) -> &'static str {
        match self {
            ConfigError::Missing => "the chat is not configured: create chat.toml in the config folder with the workspace, its channels and [[mapping]] entries",
            ConfigError::Unreadable => "a chat file exists but cannot be read: it must be a regular file this account can read",
            ConfigError::Malformed(_) => "a chat file is not valid: it must be text of at most 64 KiB, hold only the documented keys, each of the documented type",
            ConfigError::BadId => "a workspace or channel id is not a Slack id (capital letters and digits)",
            ConfigError::BadMailAfter(_) => "mail_after is not a Slack timestamp (<seconds>.<six digits>)",
            ConfigError::BadToken => "a token is not of its form: the bot token starts xoxb-, the user token xoxp- and the app token xapp- (so they differ), then letters, digits and hyphens, at most 512 bytes in all",
            ConfigError::UnknownKey(_) => "a key this file does not take is present: each chat file holds only its own documented keys",
            ConfigError::SecondWorkspace => "chat.toml's workspace is one id (a mapping names another with its own workspace key), and chat-secrets.toml holds that workspace's secrets",
            ConfigError::DuplicateChannel => "a channel is listed twice in chat.toml",
            ConfigError::FolderRepeated(_) => "two mappings name one folder (an alias of it, or the same pair twice)",
            ConfigError::ChannelRepeated(_) => "two folders map to one channel: a channel belongs to one folder",
            ConfigError::ChannelNotListed(_) => "a mapping names a channel that chat.toml's channels does not list",
            ConfigError::TooManyDelivering => "mail is on (mail_after is set), so at most 32 mappings are allowed",
            ConfigError::MappingWithoutFloor(_) => "mail is on (mail_after is set), so each mapping needs its own mail_after: the time the mapping was added",
            ConfigError::NameNotWorkspace => "a chat-secrets-<id>.toml must hold the workspace <id> its name says, and chat.toml's own workspace's secrets are chat-secrets.toml",
            ConfigError::TooManyWorkspaces => "the config folder holds secrets for more workspaces than the hub serves (8): this file is not read",
        }
    }

    /// The mapping's ordinal or the parse failure's line, when there is one.
    pub fn at(self) -> Option<usize> {
        match self {
            ConfigError::Malformed(at) | ConfigError::BadMailAfter(at) => at,
            ConfigError::FolderRepeated(n)
            | ConfigError::ChannelRepeated(n)
            | ConfigError::ChannelNotListed(n)
            | ConfigError::MappingWithoutFloor(n) => Some(n),
            _ => None,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.explain())?;
        match (self, self.at()) {
            (ConfigError::UnknownKey(Some(key)), _) => write!(f, " (key \"{key}\")"),
            (ConfigError::Malformed(_), Some(n)) => write!(f, " (line {n})"),
            (_, Some(n)) => write!(f, " (mapping {n})"),
            _ => Ok(()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// A Slack id: a capital letter, then capital letters and digits, at most 32.
pub(crate) fn is_slack_id(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_uppercase()) && s.len() <= 32 && b.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn id(v: &toml::Value) -> Result<String, ConfigError> {
    let s = v.as_str().ok_or(ConfigError::Malformed(None))?;
    if is_slack_id(s) {
        Ok(s.to_string())
    } else {
        Err(ConfigError::BadId)
    }
}

/// The line (from 1) of a parse failure, or `None` when the library gives no position.
fn parse_table(text: &str) -> Result<toml::Table, ConfigError> {
    text.parse::<toml::Table>().map_err(|e| {
        let line = e
            .span()
            .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
        ConfigError::Malformed(line)
    })
}

/// The keys each table accepts: `chat.toml`, one of its mappings, `chat-secrets.toml`.
const CONFIG_KEYS: [&str; 4] = ["workspace", "channels", "mail_after", "mapping"];
const MAPPING_KEYS: [&str; 4] = ["folder", "channel", "workspace", "mail_after"];
const SECRETS_KEYS: [&str; 4] = ["workspace", "bot_token", "app_token", "user_token"];

/// Only the accepted keys may be present. A refusal names the stray key only when another table accepts it (a key of
/// one file found in the other); a key from a file is otherwise as untrusted as a value, so it is refused unnamed.
fn only(t: &toml::Table, keys: &[&str]) -> Result<(), ConfigError> {
    match t.keys().find(|k| !keys.contains(&k.as_str())) {
        None => Ok(()),
        Some(k) => Err(ConfigError::UnknownKey(
            [&CONFIG_KEYS[..], &MAPPING_KEYS, &SECRETS_KEYS].into_iter().flatten().copied().find(|n| n == k),
        )),
    }
}

/// The workspace key: one id. A list of two or more, or a table, is a second workspace.
fn workspace(t: &toml::Table) -> Result<String, ConfigError> {
    match t.get("workspace") {
        None => Err(ConfigError::Malformed(None)),
        Some(toml::Value::Array(a)) if a.len() > 1 => Err(ConfigError::SecondWorkspace),
        Some(toml::Value::Table(_)) => Err(ConfigError::SecondWorkspace),
        Some(v) => id(v),
    }
}

/// The loaded `chat.toml`: what a folder resolves from.
#[derive(Debug)]
pub struct Config {
    workspace: String,
    /// Canonical folder, the mapping's workspace, its channel and its own `mail_after`, folders and channels one-to-one
    /// (a channel id is mapped once in the file, whatever its workspace).
    mappings: Vec<(String, String, String, Option<Ts>)>,
    /// The ordinals of the mappings whose folder did not canonicalize: kept in the file, mapped to nothing.
    ignored: Vec<usize>,
    mail_after: Option<Ts>,
}

impl Config {
    /// Read `chat.toml` from `dir` and admit its mappings through the supplied canonicalizer. Opens no other file.
    pub fn load(dir: &Path, read: Reader, canon: Canon) -> Result<Config, ConfigError> {
        Config::parse(&text(read, &dir.join(CONFIG_FILE))?, canon)
    }

    /// The file's default workspace: its top-level `workspace`, the one a mapping without its own is in, and the one
    /// whose secrets are `chat-secrets.toml`.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The admitted mappings, each as its canonical folder, its workspace (its own `workspace` key, else the file's) and
    /// its channel.
    pub fn mappings(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.mappings.iter().map(|(f, w, c, _)| (f.as_str(), w.as_str(), c.as_str()))
    }

    /// The ordinals (from 1, in file order) of the mappings the load ignored: a folder that does not exist (or is not
    /// absolute) maps nothing, and is no refusal.
    pub fn ignored(&self) -> &[usize] {
        &self.ignored
    }

    /// The hub's `mail_after`: set, the file is delivering.
    pub fn mail_after(&self) -> Option<&Ts> {
        self.mail_after.as_ref()
    }

    /// What `workspace`'s unit runs from: `credentials` and the workspace's mappings, each with its floor (`floor`).
    pub fn plan(&self, workspace: &str, credentials: Credentials) -> Plan {
        let mappings = self.mappings().filter(|(_, w, _)| *w == workspace).map(|(f, _, c)| (f.to_string(), c.to_string(), self.floor(c))).collect();
        Plan { credentials, mappings }
    }

    /// The channel's floor: the later of its mapping's `mail_after` and the hub's; `None` while the hub's is unset
    /// (and for a channel no mapping holds).
    pub fn floor(&self, channel: &str) -> Option<Ts> {
        let hub = self.mail_after.as_ref()?;
        let own = self.mappings.iter().find(|(_, _, c, _)| c == channel)?.3.as_ref();
        Some(match own {
            Some(t) if ts_key(t) > ts_key(hub) => t.clone(),
            _ => hub.clone(),
        })
    }

    pub fn parse(text: &str, canon: Canon) -> Result<Config, ConfigError> {
        let t = parse_table(text)?;
        only(&t, &CONFIG_KEYS)?;
        let workspace = workspace(&t)?;
        let mut channels = Vec::new();
        for v in t
            .get("channels")
            .and_then(toml::Value::as_array)
            .ok_or(ConfigError::Malformed(None))?
        {
            let c = id(v)?;
            if channels.contains(&c) {
                return Err(ConfigError::DuplicateChannel);
            }
            channels.push(c);
        }
        let mail_after = match t.get("mail_after") {
            None => None,
            Some(v) => Some(
                Ts::parse(v.as_str().ok_or(ConfigError::Malformed(None))?)
                    .map_err(|_| ConfigError::BadMailAfter(None))?,
            ),
        };
        let mut mappings: Vec<(String, String, String, Option<Ts>)> = Vec::new();
        let mut ignored = Vec::new();
        // Every entry's folder (canonical, else as written) and channel, ignored ones included, so the repeat checks
        // hold for a folder that is not on disk now.
        let mut seen: Vec<(String, String)> = Vec::new();
        let entries = match t.get("mapping") {
            None => &[],
            Some(v) => v.as_array().ok_or(ConfigError::Malformed(None))?.as_slice(),
        };
        for (i, m) in entries.iter().enumerate() {
            let at = i + 1;
            let m = m.as_table().ok_or(ConfigError::Malformed(None))?;
            only(m, &MAPPING_KEYS)?;
            // A mapping without its own workspace is in the file's.
            let in_workspace = match m.get("workspace") {
                Some(w) => id(w)?,
                None => workspace.clone(),
            };
            let folder = m
                .get("folder")
                .and_then(toml::Value::as_str)
                .ok_or(ConfigError::Malformed(None))?;
            let channel = id(m.get("channel").ok_or(ConfigError::Malformed(None))?)?;
            let own = match m.get("mail_after") {
                None => None,
                Some(v) => Some(
                    Ts::parse(v.as_str().ok_or(ConfigError::Malformed(None))?)
                        .map_err(|_| ConfigError::BadMailAfter(Some(at)))?,
                ),
            };
            if mail_after.is_some() && own.is_none() {
                return Err(ConfigError::MappingWithoutFloor(at));
            }
            // M1: canonical folders and channels are one-to-one and each channel is listed. A folder that does not
            // canonicalize is ignored, not refused: its row may come later, or it was removed.
            if !channels.contains(&channel) {
                return Err(ConfigError::ChannelNotListed(at));
            }
            let canonical = canon(folder);
            let key = canonical.clone().unwrap_or_else(|| folder.to_string());
            if seen.iter().any(|(f2, _)| *f2 == key) {
                return Err(ConfigError::FolderRepeated(at));
            }
            if seen.iter().any(|(_, c2)| *c2 == channel) {
                return Err(ConfigError::ChannelRepeated(at));
            }
            seen.push((key, channel.clone()));
            match canonical {
                Some(canonical) => mappings.push((canonical, in_workspace, channel, own)),
                None => ignored.push(at),
            }
        }
        if mail_after.is_some() && seen.len() > MAX_DELIVERING {
            return Err(ConfigError::TooManyDelivering);
        }
        Ok(Config {
            workspace,
            mappings,
            ignored,
            mail_after,
        })
    }
}

/// What a folder's configuration says. `mail_after` is the channel's floor (`Config::floor`). Nothing is delivered unless it is `Mapped` with a `mail_after`. `NotHub` is the
/// daemon's own answer when it is not the configured hub: it read no chat file, so it knows nothing of the folder.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Mapped {
        workspace: String,
        channel: String,
        mail_after: Option<Ts>,
    },
    Unmapped,
    Invalid(ConfigError),
    NotHub,
}

impl Resolution {
    /// Whether mail may be delivered for the folder.
    pub fn delivers(&self) -> bool {
        matches!(
            self,
            Resolution::Mapped {
                mail_after: Some(_),
                ..
            }
        )
    }

    /// A fixed explanation of the state; [`fmt::Display`] adds the place of a refusal.
    pub fn explain(&self) -> &'static str {
        match self {
            Resolution::Mapped { mail_after: Some(_), .. } => "the folder has a channel and mail is on for messages after mail_after",
            Resolution::Mapped { mail_after: None, .. } => "the folder has a channel and mail is off: set mail_after in chat.toml to a Slack timestamp to turn it on",
            Resolution::Unmapped => "the folder has no channel: add a [[mapping]] with its canonical folder and a channel to chat.toml",
            Resolution::Invalid(e) => e.explain(),
            Resolution::NotHub => "this computer is not the configured hub: the chat runs only there, and no chat file is read here",
        }
    }
}

impl fmt::Display for Resolution {
    /// The explanation, with a refusal's mapping ordinal or line, for whoever asks why a folder has no chat.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Resolution::Invalid(e) => e.fmt(f),
            _ => f.write_str(self.explain()),
        }
    }
}

/// M2: the answer for `folder` (already canonical, as the hub supplies it; matched exactly) from a load's result,
/// which may itself be a refusal. A bad load enables nothing.
pub fn resolve(config: &Result<Config, ConfigError>, folder: &str) -> Resolution {
    match config {
        Err(e) => Resolution::Invalid(*e),
        Ok(c) => match c.mappings.iter().find(|(f, _, _, _)| f == folder) {
            None => Resolution::Unmapped,
            Some((_, w, ch, _)) => Resolution::Mapped {
                workspace: w.clone(),
                channel: ch.clone(),
                mail_after: c.floor(ch),
            },
        },
    }
}

/// What one workspace's chat runs from, fixed for its life: its credentials and its mappings, each a folder, a channel and
/// the channel's floor. A reload restarts a workspace only when its plan changed, so two plans are equal exactly when a
/// running chat would run the same (its tokens byte for byte, its mappings in file order).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub credentials: Credentials,
    mappings: Vec<(String, String, Option<Ts>)>,
}

impl Plan {
    /// Each mapping: folder, channel and the channel's floor (`None`: the channel does not deliver).
    pub fn mappings(&self) -> impl Iterator<Item = (&str, &str, Option<&Ts>)> {
        self.mappings.iter().map(|(f, c, t)| (f.as_str(), c.as_str(), t.as_ref()))
    }
}

/// The loaded `chat-secrets.toml`. Its tokens format as a mask, and compare by their bytes (never formatted).
#[derive(Clone, Debug)]
pub struct Credentials {
    pub bot: Secret,
    pub app: Secret,
    /// The person's user token, which their posts go out under. Optional, so a file written before it loads: rows post
    /// and mail runs, and a person's post waits for a connect.
    pub user: Option<Secret>,
}

impl PartialEq for Credentials {
    fn eq(&self, other: &Credentials) -> bool {
        let same = |a: &Secret, b: &Secret| a.expose() == b.expose();
        same(&self.bot, &other.bot) && same(&self.app, &other.app) && self.user.as_ref().map(Secret::expose) == other.user.as_ref().map(Secret::expose)
    }
}

/// A token of the form `prefix` (the forms platform's log masker knows) then letters, digits and hyphens (the characters platform's masker reads as one token), at most 512 bytes.
fn token(t: &toml::Table, key: &str, prefix: &str) -> Result<Secret, ConfigError> {
    token_form(
        t.get(key)
            .and_then(toml::Value::as_str)
            .ok_or(ConfigError::Malformed(None))?,
        prefix,
    )
}

/// The token grammar alone, for the load and for the secrets text the daemon writes.
fn token_form(s: &str, prefix: &str) -> Result<Secret, ConfigError> {
    if s.len() > prefix.len()
        && s.len() <= 512
        && s.starts_with(prefix)
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Ok(Secret::new(s))
    } else {
        Err(ConfigError::BadToken)
    }
}

/// A pasted bot token in the load's own grammar (`BadToken` otherwise): the connect's first check, before Slack's.
pub fn bot_token(s: &str) -> Result<Secret, ConfigError> {
    token_form(s, "xoxb-")
}

/// A pasted app-level token in the load's own grammar (`BadToken` otherwise).
pub fn app_token(s: &str) -> Result<Secret, ConfigError> {
    token_form(s, "xapp-")
}

/// A pasted user token in the load's own grammar (`BadToken` otherwise).
pub fn user_token(s: &str) -> Result<Secret, ConfigError> {
    token_form(s, "xoxp-")
}

impl Credentials {
    /// One secrets file's text: its workspace and its tokens. Which workspace the file may hold is its reader's rule.
    pub fn parse(text: &str) -> Result<(String, Credentials), ConfigError> {
        let t = parse_table(text)?;
        only(&t, &SECRETS_KEYS)?;
        let workspace = workspace(&t)?;
        let credentials = Credentials {
            bot: token(&t, "bot_token", "xoxb-")?,
            app: token(&t, "app_token", "xapp-")?,
            user: t.contains_key("user_token").then(|| token(&t, "user_token", "xoxp-")).transpose()?,
        };
        Ok((workspace, credentials))
    }
}

/// The name of one workspace's secrets file, one of the two shapes: `chat-secrets.toml` (`None`, `chat.toml`'s
/// workspace) or `chat-secrets-<id>.toml`. Made only by [`Secrets::file_of`] and the listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretsFile(Option<String>);

impl SecretsFile {
    pub fn name(&self) -> String {
        self.0.as_ref().map_or_else(|| SECRETS_FILE.to_string(), |id| format!("{NAMED_PREFIX}{id}{NAMED_SUFFIX}"))
    }

    /// The `<id>` of a listed `chat-secrets-<id>.toml`; `None` for any other name (a backup, a lower-case id).
    fn named(name: &str) -> Option<&str> {
        let id = name.strip_prefix(NAMED_PREFIX)?.strip_suffix(NAMED_SUFFIX)?;
        is_slack_id(id).then_some(id)
    }
}

/// One load of the secrets files: each admitted or refused on its own, so one file never turns another workspace off.
#[derive(Debug)]
pub struct Secrets {
    /// The admitted workspaces, in workspace order, each with its file.
    admitted: BTreeMap<String, (SecretsFile, Credentials)>,
    /// Why `chat-secrets.toml` was not admitted (`Missing` when it is absent).
    default: Option<ConfigError>,
    /// Each named file refused or not read, by its name, in name order: every listed named file not admitted, those past
    /// the bound included.
    refused: Vec<(String, ConfigError)>,
}

impl Secrets {
    /// The one call that lists the config folder and opens a secrets file. `chat-secrets.toml` must hold `config`'s
    /// workspace (`SecondWorkspace`); the folder is listed once, and each name `chat-secrets-<id>.toml` whose `<id>` is a
    /// Slack id is opened in name order and must hold `<id>` (`NameNotWorkspace`, as is one whose `<id>` is `config`'s,
    /// which is not opened). The load opens at most `MAX_WORKSPACES` files; a named file past them is
    /// `TooManyWorkspaces`, not opened. A listed name that is not UTF-8 cannot match, and a listing that fails lists no
    /// named file. Every open goes through `read`.
    pub fn read(dir: &Path, read: Reader, config: &Config) -> Secrets {
        let open = |name: &str| text(read, &dir.join(name)).and_then(|t| Credentials::parse(&t));
        let mut s = Secrets { admitted: BTreeMap::new(), default: None, refused: Vec::new() };
        match open(SECRETS_FILE) {
            Ok((w, c)) if w == config.workspace => { s.admitted.insert(w, (SecretsFile(None), c)); }
            Ok(_) => s.default = Some(ConfigError::SecondWorkspace),
            Err(e) => s.default = Some(e),
        }
        let mut named = named_ids(dir);
        named.sort_unstable();
        let mut opened = 1;
        for id in named {
            let file = SecretsFile(Some(id.clone()));
            let held = if id == config.workspace {
                Err(ConfigError::NameNotWorkspace)
            } else if opened == MAX_WORKSPACES {
                Err(ConfigError::TooManyWorkspaces)
            } else {
                opened += 1;
                open(&file.name())
            };
            match held {
                Ok((w, c)) if w == id => { s.admitted.insert(w, (file, c)); }
                Ok(_) => s.refused.push((file.name(), ConfigError::NameNotWorkspace)),
                // A listed name that reads as absent (a dangling link) exists, so it is not the chat's `Missing`.
                Err(ConfigError::Missing) => s.refused.push((file.name(), ConfigError::Unreadable)),
                Err(e) => s.refused.push((file.name(), e)),
            }
        }
        s
    }

    /// The file `workspace`'s secrets are written to: `chat-secrets.toml` for `default` (`chat.toml`'s workspace), else
    /// `chat-secrets-<workspace>.toml` (`BadId` for a workspace that is not a Slack id).
    pub fn file_of(workspace: &str, default: &str) -> Result<SecretsFile, ConfigError> {
        if workspace == default {
            Ok(SecretsFile(None))
        } else if is_slack_id(workspace) {
            Ok(SecretsFile(Some(workspace.to_string())))
        } else {
            Err(ConfigError::BadId)
        }
    }

    /// The admitted workspaces, each with its file and its credentials, in workspace order.
    pub fn workspaces(&self) -> impl Iterator<Item = (&str, &SecretsFile, &Credentials)> {
        self.admitted.iter().map(|(w, (f, c))| (w.as_str(), f, c))
    }

    /// Takes `workspace`'s credentials out of the load.
    pub fn take(&mut self, workspace: &str) -> Option<Credentials> {
        self.admitted.remove(workspace).map(|(_, c)| c)
    }

    /// Why `chat-secrets.toml` was not admitted: `Missing` when it is absent. A named file's refusal is never this.
    #[cfg(test)]
    pub fn default_refusal(&self) -> Option<ConfigError> {
        self.default
    }

    /// The ids of the `chat-secrets-<id>.toml` files the folder lists, `default`'s (`chat.toml`'s workspace) left out,
    /// whether each was admitted, refused or past the bound (every listed named file is one or the other): the files a new
    /// workspace's file would sort among. Read before `take`, which moves the admitted out.
    pub fn named(&self, default: &str) -> Vec<String> {
        let admitted = self.admitted.iter().filter(|(_, (f, _))| f.0.is_some()).map(|(w, _)| w.clone());
        let refused = self.refused.iter().filter_map(|(name, _)| SecretsFile::named(name).map(str::to_string));
        let mut named: Vec<String> = admitted.chain(refused).filter(|id| id != default).collect();
        named.sort_unstable();
        named
    }

    /// Each workspace whose own file was refused, with that file's code: `chat-secrets.toml`'s under `default`
    /// (`chat.toml`'s workspace; not when the file is absent), and each named file's under the id its name says. A file's
    /// refusal is its own workspace's alone.
    pub fn refusals(&self, default: &str) -> Vec<(String, ConfigError)> {
        let default = self.default.filter(|e| *e != ConfigError::Missing).map(|e| (default.to_string(), e));
        let named = self.refused.iter().filter_map(|(name, e)| SecretsFile::named(name).map(|id| (id.to_string(), *e)));
        default.into_iter().chain(named).collect()
    }

    /// Each named file refused or not read, by its name and its code (a value of the file never).
    pub fn refused(&self) -> &[(String, ConfigError)] {
        &self.refused
    }
}

/// The `<id>`s of the `chat-secrets-<id>.toml` entries at the top of `dir`, in the listing's order; a listing that
/// fails, at its start or at any entry, gives none.
fn named_ids(dir: &Path) -> Vec<String> {
    let listed = std::fs::read_dir(dir).and_then(|entries| entries.map(|e| e.map(|e| e.file_name())).collect::<io::Result<Vec<_>>>());
    listed.unwrap_or_default().iter().filter_map(|n| n.to_str().and_then(SecretsFile::named)).map(str::to_string).collect()
}

/// A text the load admits whole: within `MAX_FILE` (else `Malformed`, as the read refuses it) and through `Config::parse`.
fn admitted(text: String, canon: Canon) -> Result<String, ConfigError> {
    if text.len() as u64 > MAX_FILE {
        return Err(ConfigError::Malformed(None));
    }
    Config::parse(&text, canon)?;
    Ok(text)
}

/// A `chat.toml` of `workspace` and no channel, which the load admits; a workspace that is not a Slack id is `BadId`.
pub fn fresh(workspace: &str) -> Result<String, ConfigError> {
    if !is_slack_id(workspace) {
        return Err(ConfigError::BadId);
    }
    let mut t = toml::Table::new();
    t.insert("workspace".into(), workspace.into());
    t.insert("channels".into(), toml::Value::Array(Vec::new()));
    admitted(t.to_string(), &|_| None)
}

/// The text of `chat-secrets.toml` for `workspace` and its three tokens, as a `Secret` (so it formats as a mask). Each
/// token passes the load's own grammar before any text is made (`BadToken`), and the text is parsed back by
/// `Credentials::parse` before it is returned.
pub fn secrets(workspace: &str, bot: &Secret, user: &Secret, app: &Secret) -> Result<Secret, ConfigError> {
    if !is_slack_id(workspace) {
        return Err(ConfigError::BadId);
    }
    token_form(bot.expose(), "xoxb-")?;
    token_form(user.expose(), "xoxp-")?;
    token_form(app.expose(), "xapp-")?;
    let mut t = toml::Table::new();
    t.insert("workspace".into(), workspace.into());
    t.insert("bot_token".into(), bot.expose().into());
    t.insert("user_token".into(), user.expose().into());
    t.insert("app_token".into(), app.expose().into());
    let text = t.to_string();
    if Credentials::parse(&text)?.0 != workspace {
        return Err(ConfigError::SecondWorkspace);
    }
    Ok(Secret::new(text))
}

/// Why a pairing was not made: the current file refused as it stands, or the edit's result refused by the load (each
/// its `ConfigError`); or the channel is mapped to another folder that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairError {
    Config(ConfigError),
    ChannelTaken,
}

impl From<ConfigError> for PairError {
    fn from(e: ConfigError) -> PairError {
        PairError::Config(e)
    }
}

/// `chat.toml`'s text with `folder` paired with `target`'s channel in `target`'s workspace, or unpaired for `None`;
/// `None` when nothing changes (the folder already maps that workspace and that channel: the same channel id in another
/// workspace is a change). A text the load refuses is refused as it stands and never rewritten. The folder's own
/// mapping (matched by its canonical spelling) is dropped; for a channel, a mapping of it in any workspace whose folder
/// does not canonicalize is dropped (taken over), one to another existing folder is `ChannelTaken`, and `{folder,
/// channel}` is appended, with `workspace` only when it is not the file's and `mail_after = floor` when the file is
/// delivering, its channel listed. A channel whose mapping the edit dropped and that no mapping holds
/// after it leaves `channels`. Ignored mappings and every other key stay; comments and key order do not. The result is
/// what the load admits, or the edit is refused with its code.
pub fn pair(
    text: &str,
    folder: &str,
    target: Option<(&str, &str)>,
    floor: Option<Ts>,
    canon: Canon,
) -> Result<Option<String>, PairError> {
    let config = Config::parse(text, canon)?;
    if target.is_some_and(|(w, c)| !is_slack_id(w) || !is_slack_id(c)) {
        return Err(ConfigError::BadId.into());
    }
    let channel = target.map(|(_, c)| c);
    let mut t = parse_table(text)?;
    let folder_key = canon(folder).unwrap_or_else(|| folder.to_string());
    let field = |m: &toml::Value, k: &str| m.get(k).and_then(toml::Value::as_str).map(str::to_string);
    let entries = match t.remove("mapping") {
        None => Vec::new(),
        Some(toml::Value::Array(a)) => a,
        Some(_) => return Err(ConfigError::Malformed(None).into()),
    };
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for m in entries {
        let (Some(f), Some(c)) = (field(&m, "folder"), field(&m, "channel")) else {
            return Err(ConfigError::Malformed(None).into());
        };
        let own = canon(&f).unwrap_or_else(|| f.clone()) == folder_key;
        let in_workspace = field(&m, "workspace").unwrap_or_else(|| config.workspace().to_string());
        if own && target == Some((in_workspace.as_str(), c.as_str())) {
            return Ok(None);
        }
        if own || channel == Some(c.as_str()) {
            if !own && canon(&f).is_some() {
                return Err(PairError::ChannelTaken);
            }
            dropped.push(c);
        } else {
            kept.push(m);
        }
    }
    if let Some((w, c)) = target {
        let mut m = toml::Table::new();
        m.insert("folder".into(), folder_key.into());
        if w != config.workspace() {
            m.insert("workspace".into(), w.into());
        }
        m.insert("channel".into(), c.into());
        if let (Some(_), Some(f)) = (config.mail_after(), &floor) {
            m.insert("mail_after".into(), f.as_str().into());
        }
        kept.push(toml::Value::Table(m));
    } else if dropped.is_empty() {
        return Ok(None);
    }
    let held: Vec<String> = kept.iter().filter_map(|m| field(m, "channel")).collect();
    let Some(toml::Value::Array(listed)) = t.get_mut("channels") else {
        return Err(ConfigError::Malformed(None).into());
    };
    listed.retain(|v| v.as_str().is_none_or(|s| !dropped.iter().any(|d| d == s) || held.iter().any(|h| h == s)));
    if let Some(c) = channel {
        if !listed.iter().any(|v| v.as_str() == Some(c)) {
            listed.push(c.into());
        }
    }
    if !kept.is_empty() {
        t.insert("mapping".into(), toml::Value::Array(kept));
    }
    Ok(Some(admitted(t.to_string(), canon)?))
}

/// One whole-file write, as [`Reader`] is the one read: a test observes it, the product's are platform's.
pub type Write<'a> = &'a dyn Fn(&Path, &[u8]) -> io::Result<()>;

/// The configuration's two writes: `chat.toml` through `config`, a secrets file through `secrets` alone, which must
/// leave the file owner-only.
#[derive(Clone, Copy)]
pub struct Writers<'a> {
    pub config: Write<'a>,
    pub secrets: Write<'a>,
}

impl Writers<'static> {
    /// Platform's durable write for `chat.toml` and its private durable write (owner-only from the temporary file's
    /// creation) for the secrets.
    pub fn platform() -> Writers<'static> {
        Writers {
            config: &sot_platform::host::write_durable,
            secrets: &sot_platform::host::write_durable_private,
        }
    }
}

/// Makes a missing `dir`, then writes the file whole through `write`.
fn save(dir: &Path, name: &str, bytes: &[u8], write: Write) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    write(&dir.join(name), bytes)
}

/// Writes `chat.toml` in `dir` whole, through the config writer.
pub fn write_config(dir: &Path, text: &str, writers: Writers) -> io::Result<()> {
    save(dir, CONFIG_FILE, text.as_bytes(), writers.config)
}

/// Writes the secrets file `file` in `dir` whole, through the private writer alone.
pub fn write_secrets(dir: &Path, file: &SecretsFile, secret: &Secret, writers: Writers) -> io::Result<()> {
    save(dir, &file.name(), secret.expose().as_bytes(), writers.secrets)
}
