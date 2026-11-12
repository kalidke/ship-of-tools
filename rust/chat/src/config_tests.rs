use crate::config::*;
use sot_platform::host::env::Env;
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

type E = ConfigError;

/// Built at run time, so no source line holds a token-shaped literal for the secret scanner.
fn bot() -> String {
    ["xoxb", "-sentinel-bot-aaaa"].concat()
}

fn app() -> String {
    ["xapp", "-sentinel-app-bbbb"].concat()
}

/// Rows' facts as a fixture: `/work/a` and its alias `/link/a` are one folder, `/work/anchor` is the inert anchor,
/// `/work/b` is another, and any other folder is unknown. The real canonicalizer is the hub landing's.
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

fn secrets_with(bot: &str, app: &str) -> String {
    format!("workspace = \"T0AAAA\"\nbot_token = \"{bot}\"\napp_token = \"{app}\"\n")
}

fn secrets() -> String {
    secrets_with(&bot(), &app())
}

fn creds(text: &str) -> Result<Credentials, ConfigError> {
    Credentials::parse(text, &parse(&chat("")).unwrap())
}

#[test]
fn mapping_refuses_aliases_duplicates_and_inert_anchor() {
    let ok = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/work/b", "C0BBBB")));
    assert!(parse(&ok).is_ok());
    // An alias of a mapped folder collides with it, though the text differs; the second entry is the one named.
    let alias = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/link/a", "C0BBBB")));
    assert_eq!(parse(&alias).unwrap_err(), E::FolderRepeated(2));
    // The same pair twice fails.
    let twice = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/work/a", "C0AAAA")));
    assert_eq!(parse(&twice).unwrap_err(), E::FolderRepeated(2));
    // One channel for two folders fails.
    let shared = chat(&(mapping("/work/a", "C0AAAA") + &mapping("/work/b", "C0AAAA")));
    assert_eq!(parse(&shared).unwrap_err(), E::ChannelRepeated(2));
    // The inert anchor takes no channel.
    assert_eq!(
        parse(&chat(&mapping("/work/anchor", "C0AAAA"))).unwrap_err(),
        E::InertAnchor(1)
    );
    // Folders match exactly: a prefix, a parent, a trailing slash and another case are not the mapped folder.
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
        &("mail_after = \"1730000000.000000\"\n".to_string() + &mapping_from("/work/a", "C0AAAA", "1730000000.000000")),
    ))
    .unwrap());
    let off = Ok(parse(&chat(&mapping("/work/a", "C0AAAA"))).unwrap());
    let bad = parse("this is not toml [");
    let missing: Result<Config, ConfigError> = Err(E::Missing);

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
    assert_eq!(r_bad, Resolution::Invalid(E::Malformed(Some(1))));
    assert_eq!(r_missing, Resolution::Invalid(E::Missing));
    // Five states, five different fixed explanations, each saying what to do.
    let all = [&r_on, &r_off, &r_unmapped, &r_bad, &r_missing].map(|r| r.explain());
    for (i, a) in all.iter().enumerate() {
        assert!(!a.is_empty());
        for b in &all[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // The person asking sees where a refusal is.
    let bad_at = parse("a = 1\nb = 2\nthis is [");
    assert!(resolve(&bad_at, "/work/a")
        .to_string()
        .ends_with("(line 3)"));
    assert_eq!(r_unmapped.to_string(), r_unmapped.explain());
    assert!(r_off.explain().contains("mail_after"));
    assert!(r_unmapped.explain().contains("[[mapping]]"));
    assert!(r_missing.explain().contains("chat.toml"));
}

/// Every variant exhaustively, so a new one is a compile error here until it has a row below.
fn index(e: ConfigError) -> usize {
    match e {
        E::Missing => 0,
        E::Unreadable => 1,
        E::Malformed(_) => 2,
        E::BadId => 3,
        E::BadMailAfter(_) => 4,
        E::BadToken => 5,
        E::UnknownKey(_) => 6,
        E::SecondWorkspace => 7,
        E::DuplicateChannel => 8,
        E::UnknownFolder(_) => 9,
        E::InertAnchor(_) => 10,
        E::FolderRepeated(_) => 11,
        E::ChannelRepeated(_) => 12,
        E::ChannelNotListed(_) => 13,
        E::TooManyDelivering => 14,
        E::MappingWithoutFloor(_) => 15,
    }
}
const VARIANTS: usize = 16;

fn mapping_from(folder: &str, channel: &str, from: &str) -> String {
    format!("{}mail_after = \"{from}\"\n", mapping(folder, channel))
}

/// Any folder is a row, canonical as spelled.
fn many_rows(folder: &str) -> Option<Folder> {
    Some(Folder { canonical: folder.to_string(), inert: false })
}

/// A file of `n` mappings, each channel listed; the hub's `mail_after` first, each mapping with its own.
fn delivering(n: usize) -> String {
    let ids: Vec<String> = (0..n).map(|i| format!("C{i:04}")).collect();
    let listed: Vec<String> = ids.iter().map(|c| format!("\"{c}\"")).collect();
    let mut text = format!("workspace = \"T0AAAA\"\nchannels = [{}]\nmail_after = \"5.000000\"\n", listed.join(", "));
    for (i, c) in ids.iter().enumerate() {
        text += &mapping_from(&format!("/work/f{i}"), c, "6.000000");
    }
    text
}

#[test]
fn a_delivering_file_has_at_most_32_mappings_and_a_file_not_delivering_has_no_bound() {
    assert_eq!(Config::parse(&delivering(32), &many_rows).map(|c| c.mappings().count()), Ok(32));
    assert_eq!(Config::parse(&delivering(33), &many_rows).map(drop), Err(E::TooManyDelivering));
    // Without the hub's `mail_after` nothing delivers, so 33 mappings load.
    let plain: String = delivering(33).lines().filter(|l| !l.starts_with("mail_after = \"5")).collect::<Vec<_>>().join("\n") + "\n";
    assert_eq!(Config::parse(&plain, &many_rows).map(|c| c.mappings().count()), Ok(33));
}

#[test]
fn a_delivering_file_refuses_a_mapping_without_its_own_floor() {
    let hub = "mail_after = \"5.000000\"\n";
    let text = chat(&(hub.to_string() + &mapping("/work/a", "C0AAAA")));
    assert_eq!(parse(&text).map(drop), Err(E::MappingWithoutFloor(1)));
    // The same file without the hub's `mail_after` loads, and has no floor.
    let c = parse(&chat(&mapping("/work/a", "C0AAAA"))).unwrap();
    assert_eq!(c.floor("C0AAAA"), None);
    assert_eq!(c.mail_after(), None);
    // A mapping's value that is not a timestamp is refused, with or without the hub's.
    for text in [chat(&mapping_from("/work/a", "C0AAAA", "soon")), chat(&(hub.to_string() + &mapping_from("/work/a", "C0AAAA", "soon")))] {
        assert_eq!(parse(&text).map(drop), Err(E::BadMailAfter(Some(1))));
    }
}

#[test]
fn a_mappings_own_mail_after_raises_only_its_channels_floor() {
    let ts = |s: &str| Some(sot_protocol::chat::Ts::parse(s).unwrap());
    // Numeric order, not text order: 10 is later than 9.
    let text = chat(&(String::from("mail_after = \"9.000000\"\n") + &mapping_from("/work/a", "C0AAAA", "10.000000") + &mapping_from("/work/b", "C0BBBB", "2.000000")));
    let c = parse(&text).unwrap();
    assert_eq!(c.floor("C0AAAA"), ts("10.000000"));
    assert_eq!(c.floor("C0BBBB"), ts("9.000000"));
    assert_eq!(c.floor("C0CCCC"), None);
    assert_eq!(c.mail_after(), ts("9.000000").as_ref());
    assert_eq!(c.mappings().collect::<Vec<_>>(), [("/work/a", "C0AAAA"), ("/work/b", "C0BBBB")]);
    let text = chat(&(String::from("mail_after = \"10.000000\"\n") + &mapping_from("/work/a", "C0AAAA", "9.000000")));
    assert_eq!(parse(&text).unwrap().floor("C0AAAA"), ts("10.000000"));
    // A mapping's value with no hub's value is no floor: nothing delivers.
    let text = chat(&mapping_from("/work/a", "C0AAAA", "10.000000"));
    assert_eq!(parse(&text).unwrap().floor("C0AAAA"), None);
    // The resolution carries the channel's floor, not the hub's value.
    let text = chat(&(String::from("mail_after = \"9.000000\"\n") + &mapping_from("/work/a", "C0AAAA", "10.000000")));
    match resolve(&parse(&text), "/work/a") {
        Resolution::Mapped { mail_after, .. } => assert_eq!(mail_after, ts("10.000000")),
        other => panic!("{other:?}"),
    }
    // The bound's number is in its explanation.
    assert!(E::TooManyDelivering.explain().contains(&MAX_DELIVERING.to_string()));
}

#[test]
fn every_refusal_has_an_input_a_code_a_place_and_its_own_explanation() {
    let m = |f: &str, c: &str| mapping(f, c);
    let two = |a: String, b: String| chat(&(a + &b));
    let long = |p: &str, n: usize| format!("{p}{}", "a".repeat(n - p.len()));
    type Outcome = Result<(), ConfigError>;
    let cases: Vec<(&str, Outcome, Outcome)> = vec![
        (
            "not toml, line 3",
            parse("a = 1\nb = 2\nthis is [").map(drop),
            Err(E::Malformed(Some(3))),
        ),
        (
            "unknown key",
            parse(&chat("typo = 1\n")).map(drop),
            Err(E::UnknownKey(None)),
        ),
        (
            "unknown key in a mapping",
            parse(&chat(&(m("/work/a", "C0AAAA") + "extra = 1\n"))).map(drop),
            Err(E::UnknownKey(None)),
        ),
        (
            "lowercase id",
            parse("workspace = \"t0aaaa\"\nchannels = []\n").map(drop),
            Err(E::BadId),
        ),
        (
            "33-byte id",
            parse(&format!(
                "workspace = \"{}\"\nchannels = []\n",
                "T".repeat(33)
            ))
            .map(drop),
            Err(E::BadId),
        ),
        (
            "32-byte id",
            parse(&format!(
                "workspace = \"{}\"\nchannels = []\n",
                "T".repeat(32)
            ))
            .map(drop),
            Ok(()),
        ),
        (
            "bad mail_after",
            parse(&chat("mail_after = \"yesterday\"\n")).map(drop),
            Err(E::BadMailAfter(None)),
        ),
        (
            "a mapping's mail_after that is not a timestamp, second mapping",
            parse(&chat(&(mapping("/work/a", "C0AAAA") + &mapping_from("/work/b", "C0BBBB", "soon")))).map(drop),
            Err(E::BadMailAfter(Some(2))),
        ),
        (
            "list of workspaces",
            parse("workspace = [\"T0AAAA\", \"T0ZZZZ\"]\nchannels = []\n").map(drop),
            Err(E::SecondWorkspace),
        ),
        (
            "duplicate channel",
            parse("workspace = \"T0AAAA\"\nchannels = [\"C0AAAA\", \"C0AAAA\"]\n").map(drop),
            Err(E::DuplicateChannel),
        ),
        (
            "unknown folder, second mapping",
            parse(&two(m("/work/a", "C0AAAA"), m("/work/zzz", "C0BBBB"))).map(drop),
            Err(E::UnknownFolder(2)),
        ),
        (
            "inert anchor",
            parse(&chat(&m("/work/anchor", "C0AAAA"))).map(drop),
            Err(E::InertAnchor(1)),
        ),
        (
            "alias",
            parse(&two(m("/work/a", "C0AAAA"), m("/link/a", "C0BBBB"))).map(drop),
            Err(E::FolderRepeated(2)),
        ),
        (
            "shared channel",
            parse(&two(m("/work/a", "C0AAAA"), m("/work/b", "C0AAAA"))).map(drop),
            Err(E::ChannelRepeated(2)),
        ),
        (
            "unlisted channel",
            parse(&chat(&m("/work/a", "C0CCCC"))).map(drop),
            Err(E::ChannelNotListed(1)),
        ),
        (
            "a delivering file with 33 mappings",
            Config::parse(&delivering(33), &many_rows).map(drop),
            Err(E::TooManyDelivering),
        ),
        (
            "a delivering file's mapping without its own floor",
            parse(&chat(&(String::from("mail_after = \"5.000000\"\n") + &mapping_from("/work/a", "C0AAAA", "6.000000") + &m("/work/b", "C0BBBB")))).map(drop),
            Err(E::MappingWithoutFloor(2)),
        ),
        (
            "bot token as the app token",
            creds(&secrets_with(&bot(), &bot())).map(drop),
            Err(E::BadToken),
        ),
        (
            "swapped tokens",
            creds(&secrets_with(&app(), &bot())).map(drop),
            Err(E::BadToken),
        ),
        (
            "bare prefix",
            creds(&secrets_with("xoxb-", &app())).map(drop),
            Err(E::BadToken),
        ),
        (
            "empty token",
            creds(&secrets_with("", &app())).map(drop),
            Err(E::BadToken),
        ),
        (
            "513-byte token",
            creds(&secrets_with(&long("xoxb-", 513), &app())).map(drop),
            Err(E::BadToken),
        ),
        (
            "space in a token",
            creds(&secrets_with(&bot().replace("sentinel", "a b"), &app())).map(drop),
            Err(E::BadToken),
        ),
        (
            "a character the masker ends a token at",
            creds(&secrets_with(&format!("{})tail", bot()), &app())).map(drop),
            Err(E::BadToken),
        ),
        (
            "id starting with a digit",
            parse("workspace = \"0AAAAA\"\nchannels = []\n").map(drop),
            Err(E::BadId),
        ),
        (
            "no channels key",
            parse("workspace = \"T0AAAA\"\n").map(drop),
            Err(E::Malformed(None)),
        ),
        (
            "a mapping with no folder",
            parse(&chat("[[mapping]]\nchannel = \"C0AAAA\"\n")).map(drop),
            Err(E::Malformed(None)),
        ),
        (
            "non-ASCII in a token",
            creds(&secrets_with(&format!("{}\u{e9}", bot()), &app())).map(drop),
            Err(E::BadToken),
        ),
        (
            "secrets for another workspace",
            creds(&secrets().replace("T0AAAA", "T0ZZZZ")).map(drop),
            Err(E::SecondWorkspace),
        ),
        (
            "secrets with an unknown key",
            creds(&(secrets() + "extra = 1\n")).map(drop),
            Err(E::UnknownKey(None)),
        ),
    ];
    let mut seen = [false; VARIANTS];
    for (what, got, want) in &cases {
        assert_eq!(got, want, "{what}");
        if let Err(e) = want {
            seen[index(*e)] = true;
        }
    }
    // The loader's own refusals.
    let none = |_: &Path| -> io::Result<String> { Err(io::ErrorKind::NotFound.into()) };
    let denied = |_: &Path| -> io::Result<String> { Err(io::ErrorKind::PermissionDenied.into()) };
    let big = |_: &Path| -> io::Result<String> { Err(io::ErrorKind::InvalidData.into()) };
    let dir = Path::new("/config/sot");
    assert_eq!(Config::load(dir, &none, &rows).unwrap_err(), E::Missing);
    assert_eq!(
        Config::load(dir, &denied, &rows).unwrap_err(),
        E::Unreadable
    );
    assert_eq!(
        Config::load(dir, &big, &rows).unwrap_err(),
        E::Malformed(None)
    );
    seen[index(E::Missing)] = true;
    seen[index(E::Unreadable)] = true;
    assert!(seen.iter().all(|s| *s), "a variant has no row: {seen:?}");
    // Boundaries admitted: a 512-byte token.
    assert!(creds(&secrets_with(&long("xoxb-", 512), &app())).is_ok());
    // Every code has a non-empty explanation, no two alike, and a place shows in the text and in no explanation.
    let codes = [
        E::Missing,
        E::Unreadable,
        E::Malformed(Some(3)),
        E::BadId,
        E::BadMailAfter(Some(2)),
        E::BadToken,
        E::UnknownKey(Some("channels")),
        E::SecondWorkspace,
        E::DuplicateChannel,
        E::UnknownFolder(2),
        E::InertAnchor(2),
        E::FolderRepeated(2),
        E::ChannelRepeated(2),
        E::ChannelNotListed(2),
        E::TooManyDelivering,
        E::MappingWithoutFloor(2),
    ];
    assert_eq!(codes.len(), VARIANTS);
    for (i, a) in codes.iter().enumerate() {
        assert_eq!(index(*a), i);
        assert!(a.explain().len() > 20, "{a:?}");
        for b in &codes[i + 1..] {
            assert_ne!(a.explain(), b.explain());
        }
    }
    assert!(E::Malformed(Some(3)).to_string().ends_with("(line 3)"));
    assert!(E::UnknownFolder(2).to_string().ends_with("(mapping 2)"));
    assert!(E::UnknownKey(Some("channels")).to_string().ends_with("(key \"channels\")"));
    assert_eq!(E::UnknownKey(None).to_string(), E::UnknownKey(None).explain());
    assert_eq!(E::Missing.to_string(), E::Missing.explain());
    // The files are named as the contract names them.
    assert_eq!(MAX_FILE, 64 * 1024);
    assert_eq!(CONFIG_FILE, "chat.toml");
    assert_eq!(SECRETS_FILE, "chat-secrets.toml");
}

#[test]
fn a_second_workspace_in_the_file_or_a_mapping_is_refused_at_load() {
    let second_in_mapping = chat(&(mapping("/work/a", "C0AAAA") + "workspace = \"T0ZZZZ\"\n"));
    assert_eq!(parse(&second_in_mapping).unwrap_err(), E::SecondWorkspace);
    let table = "channels = []\n[workspace]\nid = \"T0AAAA\"\n";
    assert_eq!(parse(table).unwrap_err(), E::SecondWorkspace);
    // A mapping may repeat the one workspace.
    assert!(parse(&chat(
        &(mapping("/work/a", "C0AAAA") + "workspace = \"T0AAAA\"\n")
    ))
    .is_ok());
    assert_eq!(
        parse(&chat("workspace = \"T0AAAA\"\n")).unwrap_err().at(),
        Some(3)
    ); // a repeated key is a parse failure, at its line
    assert_eq!(
        parse(&chat("mail_after = 5\n")).unwrap_err(),
        E::Malformed(None)
    );
}

#[test]
fn an_admitted_token_is_masked_whole_by_the_log_masker() {
    // Every character the masker ends a value at is refused, so no tail of an admitted token can print.
    let ends = ['"', '\'', ')', '\\', '&', '#', '<', '>', ' ', '\t'];
    for c in ends {
        // The character, escaped for a TOML string.
        let esc = c.escape_default().to_string().replace("\\'", "'");
        let text = secrets_with(&format!("{}{esc}x", bot()), &app());
        assert_eq!(creds(&text).unwrap_err(), E::BadToken, "{c:?}");
    }
    for token in [bot(), app(), format!("{}-A1-b2-{}", "xoxb", "z".repeat(40))] {
        let line = format!("event token={token} end");
        let shown = sot_platform::secret::redact(line.as_bytes());
        let shown = String::from_utf8_lossy(&shown);
        assert!(!shown.contains(&token[5..]), "{shown}");
    }
}

#[test]
fn a_token_prints_no_bytes_and_a_load_error_quotes_no_secret_value() {
    let c = creds(&secrets()).unwrap();
    for shown in [
        format!("{c:?}"),
        format!("{c:#?}"),
        format!("{}", c.bot),
        format!("{:?}", c.app),
    ] {
        assert!(!shown.contains("sentinel"), "{shown}");
    }
    let (b, a) = (bot(), app());
    // Malformed files with a sentinel in each place a parser might quote it.
    let bad = [
        format!("workspace = \"T0AAAA\"\nbot_token = \"{b}\napp_token = \"{a}\"\n"), // unterminated
        format!("workspace = \"T0AAAA\"\nbot_token = \"{b}\"\nbot_token = \"{b}\"\napp_token = \"{a}\"\n"), // key twice
        secrets() + &format!("extra = \"{b}\"\n"),                                     // unknown key
        format!("workspace = \"T0AAAA\"\nbot_token = [\"{b}\"]\napp_token = \"{a}\"\n"), // wrong type
        secrets_with(&format!("{b} {b}"), &a),                                        // space in a token
        secrets_with(&b, &b),                                                         // same twice
        secrets_with(&a, &b),                                                         // swapped
        secrets().replace("T0AAAA", &b),                                              // token as an id
        format!("{b} = {a}"),                                                         // bare garbage
        format!("{b}\n{a}\n[{b}\n"),                                                  // garbage over lines
    ];
    for text in &bad {
        let e = creds(text).unwrap_err();
        let shown = format!("{e} {e:?} {}", e.explain());
        assert!(!shown.contains("sentinel"), "{shown}");
    }
    // Through the file call, where the reader fails with a path and a message.
    let failing: Reader = &|p| Err(io::Error::other(format!("{} sentinel-io", p.display())));
    let e =
        Credentials::read(Path::new("/nowhere"), failing, &parse(&chat("")).unwrap()).unwrap_err();
    assert_eq!(e, E::Unreadable);
    assert!(!format!("{e} {e:?}").contains("sentinel"));
}

/// The reference's sample of a file, from the fenced block that follows `lead`. A Windows checkout holds the markdown
/// with CRLF line endings, so the search runs on the text with every line ending as LF.
fn sample_in(doc: &str, lead: &str) -> String {
    let doc = doc.replace("\r\n", "\n");
    let after = &doc[doc.find(lead).unwrap_or_else(|| panic!("the reference has no sample after {lead:?}"))..];
    let block = &after[after.find("```toml\n").unwrap() + 8..];
    block[..block.find("```").unwrap()].to_string()
}

fn sample(lead: &str) -> String {
    sample_in(include_str!("../../../docs/src/ref/config.md"), lead)
}

#[test]
fn a_sample_is_found_in_a_reference_with_either_line_ending() {
    const DOC: &str = include_str!("../../../docs/src/ref/config.md");
    let lead = "A `chat.toml` of one mapping:";
    assert_eq!(sample_in(&DOC.replace('\n', "\r\n"), lead), sample_in(DOC, lead));
}

#[test]
fn the_reference_samples_are_accepted_by_the_parsers() {
    let rows = |f: &str| (f == "/path/to/project").then(|| Folder { canonical: f.into(), inert: false });
    let config = Config::parse(&sample("A `chat.toml` of one mapping:"), &rows).unwrap();
    assert!(Credentials::parse(&sample("A `chat-secrets.toml`, with placeholder tokens:"), &config).is_ok());
}

#[test]
fn a_secrets_refusal_names_the_key_the_file_must_not_hold_and_never_a_value() {
    let b = bot();
    // A key chat.toml takes, in the secrets file: named, by the key alone.
    for key in ["channels", "mail_after"] {
        let shown = creds(&(secrets() + &format!("{key} = \"{b}\"\n"))).unwrap_err().to_string();
        assert!(shown.contains(&format!("\"{key}\"")), "{shown}");
        assert!(!shown.contains("sentinel"), "{shown}");
    }
    // A key of the secrets file, in chat.toml and in one of its mappings, is named too.
    let top = parse(&chat("bot_token = \"x\"\n")).unwrap_err();
    assert_eq!(top.to_string(), format!("{} (key \"bot_token\")", top.explain()));
    let text = chat(&(mapping("/work/a", "C0AAAA") + "bot_token = \"x\"\n"));
    assert!(parse(&text).unwrap_err().to_string().contains("\"bot_token\""));
    // A key no file accepts is never quoted, whether it looks like a token or like a form another tool writes.
    for key in [b.as_str(), "id", "token", "bot", "app"] {
        let shown = creds(&(secrets() + &format!("{key} = 1\n"))).unwrap_err();
        assert_eq!(shown, E::UnknownKey(None), "{key}");
        assert_eq!(shown.to_string(), shown.explain());
        assert_eq!(parse(&chat(&format!("{key} = 1\n"))).unwrap_err(), E::UnknownKey(None), "{key}");
    }
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
            Ok(secrets())
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    };
    let cfg = Config::load(&dir, &read, &rows).unwrap();
    assert_eq!(*opened.borrow(), vec![dir.join(CONFIG_FILE)]);
    // The secrets are opened only by their own call, and by it alone.
    assert!(Credentials::read(&dir, &read, &cfg).is_ok());
    assert_eq!(
        *opened.borrow(),
        vec![dir.join(CONFIG_FILE), dir.join(SECRETS_FILE)]
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

/// A scratch folder that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("sot-chat-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_real_read_refuses_by_kind_and_the_loader_by_code() {
    let s = Scratch::new("read");
    let file = |name: &str, bytes: &[u8]| {
        let p = s.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let at_cap = vec![b'#'; MAX_FILE as usize];
    assert_eq!(
        read_file(&file("at", &at_cap)).unwrap().len(),
        MAX_FILE as usize
    );
    let over = vec![b'#'; MAX_FILE as usize + 1];
    assert_eq!(
        read_file(&file("over", &over)).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        read_file(&file("utf", &[0xff, 0xfe])).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    // Through the loader, the oversize file is a refusal and the file's text is not in it.
    std::fs::write(s.0.join(CONFIG_FILE), &over).unwrap();
    assert_eq!(
        Config::load(&s.0, &read_file, &rows).unwrap_err(),
        E::Malformed(None)
    );
    // A directory where the file belongs is unreadable, by platform's refusal of anything but a regular file.
    std::fs::remove_file(s.0.join(CONFIG_FILE)).unwrap();
    std::fs::create_dir(s.0.join(CONFIG_FILE)).unwrap();
    assert_eq!(
        Config::load(&s.0, &read_file, &rows).unwrap_err(),
        E::Unreadable
    );
}
