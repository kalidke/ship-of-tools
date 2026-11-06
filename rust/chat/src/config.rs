//! The chat's configuration: `chat.toml` (the one workspace, its channels, the folder-to-channel mappings, an optional
//! `mail_after`) and `chat-secrets.toml` (the workspace and its two tokens). Both sit in platform's config folder. Every
//! refusal is a fixed code with a fixed explanation: a value from either file is never quoted, and the library's own
//! parse errors (which quote the source) are never formatted. Reading the secrets is its own call, so a caller that is
//! not the hub never opens that file.

use crate::http::Secret;
use sot_platform::host::env::Env;
use sot_protocol::chat::Ts;
use std::collections::HashSet;
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

pub fn read_file(path: &Path) -> io::Result<String> {
    std::fs::read_to_string(path)
}

/// `$XDG_CONFIG_HOME/sot` (or the platform's own place), by platform's resolver; `None` when no base variable is set.
pub fn config_dir(env: &Env) -> Option<PathBuf> {
    sot_platform::host::state_dir::sot_config_dir_in(env)
}

pub const CONFIG_FILE: &str = "chat.toml";
pub const SECRETS_FILE: &str = "chat-secrets.toml";

/// Why a file was refused. A code: no value from the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing,
    Unreadable,
    Malformed,
    BadId,
    BadMailAfter,
    BadToken,
    SecondWorkspace,
    DuplicateChannel,
    DuplicateToken,
    UnknownFolder,
    InertAnchor,
    FolderRepeated,
    ChannelRepeated,
    ChannelNotListed,
}

impl ConfigError {
    pub fn explain(self) -> &'static str {
        match self {
            ConfigError::Missing => "the chat is not configured: create chat.toml in the config folder with the workspace, its channels and [[mapping]] entries",
            ConfigError::Unreadable => "a chat file exists but cannot be read: check its permissions",
            ConfigError::Malformed => "a chat file is not valid: it must hold only the documented keys, each of the documented type",
            ConfigError::BadId => "a workspace or channel id is not a Slack id (capital letters and digits)",
            ConfigError::BadMailAfter => "mail_after is not a Slack timestamp (<seconds>.<six digits>)",
            ConfigError::BadToken => "a token in chat-secrets.toml is empty, too long or holds a space or control character",
            ConfigError::SecondWorkspace => "the chat serves one workspace: a second one, or secrets for another, is refused",
            ConfigError::DuplicateChannel => "a channel is listed twice in chat.toml",
            ConfigError::DuplicateToken => "the bot token and the app token are the same",
            ConfigError::UnknownFolder => "a mapping names a folder no row knows: use the project folder's canonical path",
            ConfigError::InertAnchor => "a mapping names the default anchor, which runs nothing and cannot take a channel",
            ConfigError::FolderRepeated => "two mappings name one folder (an alias of it, or the same pair twice)",
            ConfigError::ChannelRepeated => "two folders map to one channel: a channel belongs to one folder",
            ConfigError::ChannelNotListed => "a mapping names a channel that chat.toml's channels does not list",
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.explain())
    }
}

impl std::error::Error for ConfigError {}

/// A Slack id: a capital letter, then capital letters and digits, at most 32.
fn id(v: &toml::Value) -> Result<String, ConfigError> {
    let s = v.as_str().ok_or(ConfigError::Malformed)?;
    let mut b = s.bytes();
    let ok = b.next().is_some_and(|c| c.is_ascii_uppercase())
        && s.len() <= 32
        && b.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if ok {
        Ok(s.to_string())
    } else {
        Err(ConfigError::BadId)
    }
}

fn parse_table(text: &str) -> Result<toml::Table, ConfigError> {
    text.parse::<toml::Table>()
        .map_err(|_| ConfigError::Malformed)
}

/// Only the named keys may be present.
fn only(t: &toml::Table, keys: &[&str]) -> Result<(), ConfigError> {
    if t.keys().all(|k| keys.contains(&k.as_str())) {
        Ok(())
    } else {
        Err(ConfigError::Malformed)
    }
}

/// The workspace key: one id. A list of two or more, or a table, is a second workspace.
fn workspace(t: &toml::Table) -> Result<String, ConfigError> {
    match t.get("workspace") {
        None => Err(ConfigError::Malformed),
        Some(toml::Value::Array(a)) if a.len() > 1 => Err(ConfigError::SecondWorkspace),
        Some(toml::Value::Table(_)) => Err(ConfigError::SecondWorkspace),
        Some(v) => id(v),
    }
}

struct Mapping {
    folder: String,
    channel: String,
}

/// The loaded `chat.toml`.
#[derive(Debug)]
pub struct Config {
    workspace: String,
    channels: Vec<String>,
    mappings: Vec<(String, String)>,
    mail_after: Option<Ts>,
}

impl Config {
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn channels(&self) -> &[String] {
        &self.channels
    }

    pub fn mail_after(&self) -> Option<&Ts> {
        self.mail_after.as_ref()
    }

    /// Read `chat.toml` from `dir` and admit its mappings against the supplied row facts. Opens no other file.
    pub fn load(dir: &Path, read: Reader, rows: Rows) -> Result<Config, ConfigError> {
        match read(&dir.join(CONFIG_FILE)) {
            Ok(text) => Config::parse(&text, rows),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(ConfigError::Missing),
            Err(_) => Err(ConfigError::Unreadable),
        }
    }

    pub fn parse(text: &str, rows: Rows) -> Result<Config, ConfigError> {
        let t = parse_table(text)?;
        only(&t, &["workspace", "channels", "mail_after", "mapping"])?;
        let workspace = workspace(&t)?;
        let mut channels = Vec::new();
        for v in t
            .get("channels")
            .and_then(toml::Value::as_array)
            .ok_or(ConfigError::Malformed)?
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
                Ts::parse(v.as_str().ok_or(ConfigError::Malformed)?)
                    .map_err(|_| ConfigError::BadMailAfter)?,
            ),
        };
        let mut entries = Vec::new();
        if let Some(v) = t.get("mapping") {
            for m in v.as_array().ok_or(ConfigError::Malformed)? {
                let m = m.as_table().ok_or(ConfigError::Malformed)?;
                only(m, &["folder", "channel", "workspace"])?;
                if let Some(w) = m.get("workspace") {
                    if id(w)? != workspace {
                        return Err(ConfigError::SecondWorkspace);
                    }
                }
                entries.push(Mapping {
                    folder: m
                        .get("folder")
                        .and_then(toml::Value::as_str)
                        .ok_or(ConfigError::Malformed)?
                        .to_string(),
                    channel: id(m.get("channel").ok_or(ConfigError::Malformed)?)?,
                });
            }
        }
        let mappings = admit(entries, &channels, rows)?;
        Ok(Config {
            workspace,
            channels,
            mappings,
            mail_after,
        })
    }
}

/// M1: canonical folders and channels are one-to-one, no folder is the inert anchor, and each channel is listed.
fn admit(
    entries: Vec<Mapping>,
    channels: &[String],
    rows: Rows,
) -> Result<Vec<(String, String)>, ConfigError> {
    let mut folders = HashSet::new();
    let mut used = HashSet::new();
    let mut out = Vec::new();
    for m in entries {
        let f = rows(&m.folder).ok_or(ConfigError::UnknownFolder)?;
        if f.inert {
            return Err(ConfigError::InertAnchor);
        }
        if !channels.contains(&m.channel) {
            return Err(ConfigError::ChannelNotListed);
        }
        if !folders.insert(f.canonical.clone()) {
            return Err(ConfigError::FolderRepeated);
        }
        if !used.insert(m.channel.clone()) {
            return Err(ConfigError::ChannelRepeated);
        }
        out.push((f.canonical, m.channel));
    }
    Ok(out)
}

/// What a folder's configuration says. Nothing is delivered unless it is `Mapped` with a `mail_after`. The hub landing
/// adds the case of a daemon that is not the hub.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Mapped {
        workspace: String,
        channel: String,
        mail_after: Option<Ts>,
    },
    Unmapped,
    Invalid(ConfigError),
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

    /// A fixed explanation of the state, for whoever asks why a folder has no chat.
    pub fn explain(&self) -> &'static str {
        match self {
            Resolution::Mapped { mail_after: Some(_), .. } => "the folder has a channel and mail is on for messages after mail_after",
            Resolution::Mapped { mail_after: None, .. } => "the folder has a channel and mail is off: set mail_after in chat.toml to a Slack timestamp to turn it on",
            Resolution::Unmapped => "the folder has no channel: add a [[mapping]] with its canonical folder and a channel to chat.toml",
            Resolution::Invalid(e) => e.explain(),
        }
    }
}

/// M2: the answer for `folder` (already canonical, as the hub supplies it; matched exactly) from a load's result,
/// which may itself be a refusal. A bad load enables nothing.
pub fn resolve(config: &Result<Config, ConfigError>, folder: &str) -> Resolution {
    match config {
        Err(e) => Resolution::Invalid(*e),
        Ok(c) => match c.mappings.iter().find(|(f, _)| f == folder) {
            None => Resolution::Unmapped,
            Some((_, ch)) => Resolution::Mapped {
                workspace: c.workspace.clone(),
                channel: ch.clone(),
                mail_after: c.mail_after.clone(),
            },
        },
    }
}

/// The loaded `chat-secrets.toml`. Its tokens format as a mask.
#[derive(Debug)]
pub struct Credentials {
    pub workspace: String,
    pub bot: Secret,
    pub app: Secret,
}

fn token(t: &toml::Table, key: &str) -> Result<String, ConfigError> {
    let s = t
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or(ConfigError::Malformed)?;
    if s.is_empty() || s.len() > 512 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ConfigError::BadToken);
    }
    Ok(s.to_string())
}

impl Credentials {
    /// Read `chat-secrets.toml` from `dir`. The only call that opens it; the workspace must be `config`'s.
    pub fn read(dir: &Path, read: Reader, config: &Config) -> Result<Credentials, ConfigError> {
        match read(&dir.join(SECRETS_FILE)) {
            Ok(text) => Credentials::parse(&text, config),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(ConfigError::Missing),
            Err(_) => Err(ConfigError::Unreadable),
        }
    }

    pub fn parse(text: &str, config: &Config) -> Result<Credentials, ConfigError> {
        let t = parse_table(text)?;
        only(&t, &["workspace", "bot_token", "app_token"])?;
        let workspace = workspace(&t)?;
        if workspace != config.workspace {
            return Err(ConfigError::SecondWorkspace);
        }
        let (bot, app) = (token(&t, "bot_token")?, token(&t, "app_token")?);
        if bot == app {
            return Err(ConfigError::DuplicateToken);
        }
        Ok(Credentials {
            workspace,
            bot: Secret::new(bot),
            app: Secret::new(app),
        })
    }
}
