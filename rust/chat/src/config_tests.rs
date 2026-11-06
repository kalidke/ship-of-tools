use crate::config::*;
use sot_platform::host::env::Env;
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

const BOT: &str = "sentinel-bot-token-aaaa";
const APP: &str = "sentinel-app-token-bbbb";

/// Rows' facts as a fixture: `/work/a` and its alias `/link/a` are one folder, `/work/anchor` is the inert anchor,
/// `/work/b` is another, and any other folder is unknown.
fn rows(folder: &str) -> Option<Folder> {
    let (canonical, inert) = match folder {
        "/work/a" | "/link/a" => ("/work/a", false),
        "/work/b" => ("/work/b", false),
        "/work/anchor" => ("/work/anchor", true),
        _ => return None,
    };
    Some(Folder {
        canonical: canonical.to_string(),
        inert,
    })
}

fn chat(extra: &str) -> String {
    format!("workspace = \"T0AAAA\"\nchannels = [\"C0AAAA\", \"C0BBBB\"]\n{extra}")
}

fn mapping(folder: &str, channel: &str) -> String {
    format!("[[mapping]]\nfolder = \"{folder}\"\nchannel = \"{channel}\"\n")
}

fn parse(text: &str) -> Result<Config, ConfigError> {
    Config::parse(text, &rows)
}

fn secrets(extra: &str) -> String {
    format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT}\"\napp_token = \"{APP}\"\n{extra}")
}

#[test]
fn mapping_refuses_aliases_duplicates_and_inert_anchor() {
    let ok = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/work/b", "C0BBBB")));
    assert!(parse(&ok).is_ok());
    // An alias of a mapped folder collides with it, though the text differs.
    let alias = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/link/a", "C0BBBB")));
    assert_eq!(parse(&alias).unwrap_err(), ConfigError::FolderRepeated);
    // The same pair twice fails.
    let twice = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/work/a", "C0AAAA")));
    assert_eq!(parse(&twice).unwrap_err(), ConfigError::FolderRepeated);
    // One channel for two folders fails.
    let shared = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/work/b", "C0AAAA")));
    assert_eq!(parse(&shared).unwrap_err(), ConfigError::ChannelRepeated);
    // The inert anchor takes no channel.
    let anchor = chat(&mapping("/work/anchor", "C0AAAA"));
    assert_eq!(parse(&anchor).unwrap_err(), ConfigError::InertAnchor);
    // A folder no row knows, and a channel the file does not list.
    assert_eq!(
        parse(&chat(&mapping("/work/zzz", "C0AAAA"))).unwrap_err(),
        ConfigError::UnknownFolder
    );
    assert_eq!(
        parse(&chat(&mapping("/work/a", "C0CCCC"))).unwrap_err(),
        ConfigError::ChannelNotListed
    );
    // Folders match exactly: a prefix, a parent and a trailing slash are not the mapped folder.
    let cfg = Ok(parse(&ok).unwrap());
    assert!(matches!(
        resolve(&cfg, "/work/a"),
        Resolution::Mapped { .. }
    ));
    for other in ["/work/a/", "/work", "/work/a/sub", "/link/a", "/WORK/A", ""] {
        assert_eq!(resolve(&cfg, other), Resolution::Unmapped, "{other}");
    }
    assert_eq!(resolve(&cfg, "/work/anchor"), Resolution::Unmapped);
}

#[test]
fn resolve_explains_unmapped_invalid_and_disabled_delivery() {
    let on = Ok(parse(&chat(
        &("mail_after = \"1730000000.000000\"\n".to_string() + &mapping("/work/a", "C0AAAA")),
    ))
    .unwrap());
    let off = Ok(parse(&chat(&mapping("/work/a", "C0AAAA"))).unwrap());
    let bad = parse("this is not toml [");
    let missing: Result<Config, ConfigError> = Err(ConfigError::Missing);

    let r_on = resolve(&on, "/work/a");
    let r_off = resolve(&off, "/work/a");
    let r_unmapped = resolve(&on, "/work/b");
    let r_bad = resolve(&bad, "/work/a");
    let r_missing = resolve(&missing, "/work/a");

    assert!(r_on.delivers());
    assert_eq!(
        r_off,
        Resolution::Mapped {
            workspace: "T0AAAA".into(),
            channel: "C0AAAA".into(),
            mail_after: None
        }
    );
    // Only a mapped folder with a mail_after delivers.
    for r in [&r_off, &r_unmapped, &r_bad, &r_missing] {
        assert!(!r.delivers());
    }
    assert_eq!(r_unmapped, Resolution::Unmapped);
    assert_eq!(r_bad, Resolution::Invalid(ConfigError::Malformed));
    assert_eq!(r_missing, Resolution::Invalid(ConfigError::Missing));
    // Five states, five different fixed explanations, each saying what to do.
    let all = [&r_on, &r_off, &r_unmapped, &r_bad, &r_missing].map(|r| r.explain());
    for (i, a) in all.iter().enumerate() {
        assert!(!a.is_empty());
        for b in &all[i + 1..] {
            assert_ne!(a, b);
        }
    }
    assert!(r_off.explain().contains("mail_after"));
    assert!(r_unmapped.explain().contains("[[mapping]]"));
    assert!(r_missing.explain().contains("chat.toml"));
}

#[test]
fn a_second_workspace_duplicates_and_bad_values_are_refused_at_load() {
    let m = mapping("/work/a", "C0AAAA");
    let second_in_mapping = chat(&format!("{m}workspace = \"T0ZZZZ\"\n")); // a key after [[mapping]] belongs to it
    assert_eq!(
        parse(&second_in_mapping).unwrap_err(),
        ConfigError::SecondWorkspace
    );
    let list = "workspace = [\"T0AAAA\", \"T0ZZZZ\"]\nchannels = [\"C0AAAA\"]\n";
    assert_eq!(parse(list).unwrap_err(), ConfigError::SecondWorkspace);
    let table = "channels = []\n[workspace]\nid = \"T0AAAA\"\n";
    assert_eq!(parse(table).unwrap_err(), ConfigError::SecondWorkspace);
    let dup = "workspace = \"T0AAAA\"\nchannels = [\"C0AAAA\", \"C0AAAA\"]\n";
    assert_eq!(parse(dup).unwrap_err(), ConfigError::DuplicateChannel);
    // A duplicate key is not valid TOML.
    assert_eq!(
        parse(&chat("workspace = \"T0AAAA\"\n")).unwrap_err(),
        ConfigError::Malformed
    );
    assert_eq!(
        parse(&chat("typo = 1\n")).unwrap_err(),
        ConfigError::Malformed
    );
    assert_eq!(
        parse("workspace = \"t0aaaa\"\nchannels = []\n").unwrap_err(),
        ConfigError::BadId
    );
    assert_eq!(
        parse(&chat("mail_after = \"yesterday\"\n")).unwrap_err(),
        ConfigError::BadMailAfter
    );
    assert_eq!(
        parse(&chat("mail_after = 5\n")).unwrap_err(),
        ConfigError::Malformed
    );
    // Secrets: the same token twice, and secrets for another workspace.
    let cfg = parse(&chat("")).unwrap();
    let same = format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT}\"\napp_token = \"{BOT}\"\n");
    assert_eq!(
        Credentials::parse(&same, &cfg).unwrap_err(),
        ConfigError::DuplicateToken
    );
    let other = secrets("").replace("T0AAAA", "T0ZZZZ");
    assert_eq!(
        Credentials::parse(&other, &cfg).unwrap_err(),
        ConfigError::SecondWorkspace
    );
    assert!(Credentials::parse(&secrets(""), &cfg).is_ok());
}

#[test]
fn a_token_prints_no_bytes_and_a_load_error_quotes_no_secret_value() {
    let cfg = parse(&chat("")).unwrap();
    let creds = Credentials::parse(&secrets(""), &cfg).unwrap();
    for shown in [
        format!("{creds:?}"),
        format!("{:#?}", creds),
        format!("{}", creds.bot),
        format!("{:?}", creds.app),
    ] {
        assert!(!shown.contains("sentinel"), "{shown}");
    }
    // Malformed files with a sentinel in each place a parser might quote it.
    let bad = [
        format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT}\napp_token = \"{APP}\"\n"), // unterminated
        format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT}\"\nbot_token = \"{BOT}\"\napp_token = \"{APP}\"\n"), // key twice
        secrets(&format!("extra = \"{BOT}\"\n")),                                                  // unknown key
        format!("workspace = \"T0AAAA\"\nbot_token = [\"{BOT}\"]\napp_token = \"{APP}\"\n"),      // wrong type
        format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT} {BOT}\"\napp_token = \"{APP}\"\n"),   // space in a token
        format!("workspace = \"T0AAAA\"\nbot_token = \"{BOT}\"\napp_token = \"{BOT}\"\n"),         // same twice
        secrets("").replace("T0AAAA", &format!("{BOT}")),                                          // token as an id
        format!("{BOT} = {APP}"),                                                                  // bare garbage
    ];
    for text in &bad {
        let e = Credentials::parse(text, &cfg).unwrap_err();
        let shown = format!("{e} {e:?} {}", e.explain());
        assert!(!shown.contains("sentinel"), "{shown}");
    }
    // The same through the file call, where the reader fails with a path and a message.
    let failing: Reader = &|p| Err(io::Error::other(format!("{} sentinel-io", p.display())));
    let e = Credentials::read(Path::new("/nowhere"), failing, &cfg).unwrap_err();
    assert_eq!(e, ConfigError::Unreadable);
    assert!(!format!("{e} {e:?}").contains("sentinel"));
}

#[test]
fn loading_the_configuration_opens_no_secret_file() {
    let dir = PathBuf::from("/config/sot");
    let opened = RefCell::new(Vec::new());
    let read = |p: &Path| -> io::Result<String> {
        opened.borrow_mut().push(p.to_path_buf());
        if p.ends_with(CONFIG_FILE) {
            Ok(chat(&mapping("/work/a", "C0AAAA")))
        } else if p.ends_with(SECRETS_FILE) {
            Ok(secrets(""))
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    };
    let cfg = Config::load(&dir, &read, &rows).unwrap();
    assert_eq!(*opened.borrow(), vec![dir.join(CONFIG_FILE)]);
    // The secrets are opened only by their own call, and by it alone.
    let creds = Credentials::read(&dir, &read, &cfg).unwrap();
    assert_eq!(creds.workspace, "T0AAAA");
    assert_eq!(
        *opened.borrow(),
        vec![dir.join(CONFIG_FILE), dir.join(SECRETS_FILE)]
    );
    // A missing file is its own state, and so is one that cannot be read.
    let none = |_: &Path| -> io::Result<String> { Err(io::ErrorKind::NotFound.into()) };
    assert_eq!(
        Config::load(&dir, &none, &rows).unwrap_err(),
        ConfigError::Missing
    );
    assert_eq!(
        Credentials::read(&dir, &none, &cfg).unwrap_err(),
        ConfigError::Missing
    );
    let denied = |_: &Path| -> io::Result<String> { Err(io::ErrorKind::PermissionDenied.into()) };
    assert_eq!(
        Config::load(&dir, &denied, &rows).unwrap_err(),
        ConfigError::Unreadable
    );
}

#[test]
fn the_files_are_named_in_platforms_config_folder() {
    let env = Env::empty()
        .with("XDG_CONFIG_HOME", "/xdg")
        .with("HOME", "/home/x");
    if !cfg!(windows) {
        assert_eq!(config_dir(&env), Some(PathBuf::from("/xdg/sot")));
        assert_eq!(
            config_dir(&Env::empty().with("HOME", "/home/x")),
            Some(PathBuf::from("/home/x/.config/sot"))
        );
    }
    assert_eq!(config_dir(&Env::empty()), None);
}
