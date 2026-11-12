//! The chat's configuration: `chat.toml` (the one workspace, its channels, the folder-to-channel mappings, an optional
//! `mail_after`) and `chat-secrets.toml` (the workspace and its two tokens). Both sit in platform's config folder. Every
//! refusal is a fixed code with a fixed explanation: a value from either file is never quoted, a key is quoted only
//! when a table of these files accepts it (the caller says which file refused), and the library's own parse errors
//! (which quote the source) are never formatted. Reading the secrets is its own call, so a caller that is
//! not the hub never opens that file.

use crate::http::Secret;
use crate::normalize::ts_key;
use sot_platform::host::env::Env;
use sot_protocol::chat::Ts;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// What rows knows of a folder the file names: its canonical spelling and whether it is the inert default anchor.
/// Chat does not resolve a row; the daemon's adapter supplies this.
pub struct Folder {
    pub canonical: String,
    pub inert: bool,
}

/// The supplied facts: `None` when no row knows the folder.
pub type Rows<'a> = &'a dyn Fn(&str) -> Option<Folder>;

/// The one file read, so a test (and the hub's guard) sees which files are opened.
pub type Reader<'a> = &'a dyn Fn(&Path) -> io::Result<String>;

/// The most either file may hold. The cap's job is to end a stall and bound the work a planted file can cause (the
/// channel check is quadratic, and each mapping asks rows); 64 KiB holds thousands of mappings.
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
pub const SECRETS_FILE: &str = "chat-secrets.toml";

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
    UnknownFolder(usize),
    InertAnchor(usize),
    FolderRepeated(usize),
    ChannelRepeated(usize),
    ChannelNotListed(usize),
    TooManyDelivering,
    MappingWithoutFloor(usize),
}

impl ConfigError {
    pub fn explain(self) -> &'static str {
        match self {
            ConfigError::Missing => "the chat is not configured: create chat.toml in the config folder with the workspace, its channels and [[mapping]] entries",
            ConfigError::Unreadable => "a chat file exists but cannot be read: it must be a regular file this account can read",
            ConfigError::Malformed(_) => "a chat file is not valid: it must be text of at most 64 KiB, hold only the documented keys, each of the documented type",
            ConfigError::BadId => "a workspace or channel id is not a Slack id (capital letters and digits)",
            ConfigError::BadMailAfter(_) => "mail_after is not a Slack timestamp (<seconds>.<six digits>)",
            ConfigError::BadToken => "a token is not of its form: the bot token starts xoxb- and the app token xapp- (so they differ), then letters, digits and hyphens, at most 512 bytes in all",
            ConfigError::UnknownKey(_) => "a key this file does not take is present: each chat file holds only its own documented keys",
            ConfigError::SecondWorkspace => "the chat serves one workspace: a second one, or secrets for another, is refused",
            ConfigError::DuplicateChannel => "a channel is listed twice in chat.toml",
            ConfigError::UnknownFolder(_) => "a mapping names a folder no row knows: use the project folder's canonical path",
            ConfigError::InertAnchor(_) => "a mapping names the default anchor, which runs nothing and cannot take a channel",
            ConfigError::FolderRepeated(_) => "two mappings name one folder (an alias of it, or the same pair twice)",
            ConfigError::ChannelRepeated(_) => "two folders map to one channel: a channel belongs to one folder",
            ConfigError::ChannelNotListed(_) => "a mapping names a channel that chat.toml's channels does not list",
            ConfigError::TooManyDelivering => "mail is on (mail_after is set), so at most 32 mappings are allowed",
            ConfigError::MappingWithoutFloor(_) => "mail is on (mail_after is set), so each mapping needs its own mail_after: the time the mapping was added",
        }
    }

    /// The mapping's ordinal or the parse failure's line, when there is one.
    pub fn at(self) -> Option<usize> {
        match self {
            ConfigError::Malformed(at) | ConfigError::BadMailAfter(at) => at,
            ConfigError::UnknownFolder(n)
            | ConfigError::InertAnchor(n)
            | ConfigError::FolderRepeated(n)
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
const SECRETS_KEYS: [&str; 3] = ["workspace", "bot_token", "app_token"];

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
    /// Canonical folder, channel and the mapping's own `mail_after`, folders and channels one-to-one.
    mappings: Vec<(String, String, Option<Ts>)>,
    mail_after: Option<Ts>,
}

impl Config {
    /// Read `chat.toml` from `dir` and admit its mappings against the supplied row facts. Opens no other file.
    pub fn load(dir: &Path, read: Reader, rows: Rows) -> Result<Config, ConfigError> {
        Config::parse(&text(read, &dir.join(CONFIG_FILE))?, rows)
    }

    /// The one configured workspace id.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The admitted mappings, each as its canonical folder and its channel.
    pub fn mappings(&self) -> impl Iterator<Item = (&str, &str)> {
        self.mappings.iter().map(|(f, c, _)| (f.as_str(), c.as_str()))
    }

    /// The hub's `mail_after`: set, the file is delivering.
    pub fn mail_after(&self) -> Option<&Ts> {
        self.mail_after.as_ref()
    }

    /// The channel's floor: the later of its mapping's `mail_after` and the hub's; `None` while the hub's is unset
    /// (and for a channel no mapping holds).
    pub fn floor(&self, channel: &str) -> Option<Ts> {
        let hub = self.mail_after.as_ref()?;
        let own = self.mappings.iter().find(|(_, c, _)| c == channel)?.2.as_ref();
        Some(match own {
            Some(t) if ts_key(t) > ts_key(hub) => t.clone(),
            _ => hub.clone(),
        })
    }

    pub fn parse(text: &str, rows: Rows) -> Result<Config, ConfigError> {
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
        let mut mappings: Vec<(String, String, Option<Ts>)> = Vec::new();
        let entries = match t.get("mapping") {
            None => &[],
            Some(v) => v.as_array().ok_or(ConfigError::Malformed(None))?.as_slice(),
        };
        for (i, m) in entries.iter().enumerate() {
            let at = i + 1;
            let m = m.as_table().ok_or(ConfigError::Malformed(None))?;
            only(m, &MAPPING_KEYS)?;
            if let Some(w) = m.get("workspace") {
                if id(w)? != workspace {
                    return Err(ConfigError::SecondWorkspace);
                }
            }
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
            // M1: canonical folders and channels are one-to-one, the inert anchor takes none, each channel is listed.
            let f = rows(folder).ok_or(ConfigError::UnknownFolder(at))?;
            if f.inert {
                return Err(ConfigError::InertAnchor(at));
            }
            if !channels.contains(&channel) {
                return Err(ConfigError::ChannelNotListed(at));
            }
            if mappings.iter().any(|(f2, _, _)| *f2 == f.canonical) {
                return Err(ConfigError::FolderRepeated(at));
            }
            if mappings.iter().any(|(_, c2, _)| *c2 == channel) {
                return Err(ConfigError::ChannelRepeated(at));
            }
            mappings.push((f.canonical, channel, own));
        }
        if mail_after.is_some() && mappings.len() > MAX_DELIVERING {
            return Err(ConfigError::TooManyDelivering);
        }
        Ok(Config {
            workspace,
            mappings,
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
        Ok(c) => match c.mappings.iter().find(|(f, _, _)| f == folder) {
            None => Resolution::Unmapped,
            Some((_, ch, _)) => Resolution::Mapped {
                workspace: c.workspace.clone(),
                channel: ch.clone(),
                mail_after: c.floor(ch),
            },
        },
    }
}

/// The loaded `chat-secrets.toml`. Its tokens format as a mask.
#[derive(Debug)]
pub struct Credentials {
    pub bot: Secret,
    pub app: Secret,
}

/// A token of the form `prefix` (the forms platform's log masker knows) then letters, digits and hyphens (the characters platform's masker reads as one token), at most 512 bytes.
fn token(t: &toml::Table, key: &str, prefix: &str) -> Result<Secret, ConfigError> {
    let s = t
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or(ConfigError::Malformed(None))?;
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

impl Credentials {
    /// Read `chat-secrets.toml` from `dir`. The only call that opens it; the workspace must be `config`'s.
    pub fn read(dir: &Path, read: Reader, config: &Config) -> Result<Credentials, ConfigError> {
        Credentials::parse(&text(read, &dir.join(SECRETS_FILE))?, config)
    }

    pub fn parse(text: &str, config: &Config) -> Result<Credentials, ConfigError> {
        let t = parse_table(text)?;
        only(&t, &SECRETS_KEYS)?;
        if workspace(&t)? != config.workspace {
            return Err(ConfigError::SecondWorkspace);
        }
        Ok(Credentials {
            bot: token(&t, "bot_token", "xoxb-")?,
            app: token(&t, "app_token", "xapp-")?,
        })
    }
}
