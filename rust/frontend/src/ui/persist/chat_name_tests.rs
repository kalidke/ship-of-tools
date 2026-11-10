use super::*;

/// A fresh empty folder under the temporary one, named for its test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sot-chat-name-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An environment whose config folder is under `base`, on any system.
fn env_in(base: &Path) -> Env {
    if cfg!(windows) { Env::empty().with("LOCALAPPDATA", base) } else { Env::empty().with("XDG_CONFIG_HOME", base) }
}

const FILE: &str = "# my settings\n[trust]\nroot_prefix = \"/work\"  # kept\n\n[layout]\npreset = \"laptop\"\n";

#[test]
fn a_name_is_checked_before_it_is_saved() {
    assert_eq!(check_chat_name("  Ada L  ").as_deref(), Ok("Ada L"), "trimmed");
    assert!(check_chat_name("").unwrap_err().contains("empty"));
    assert!(check_chat_name("  \t ").unwrap_err().contains("empty"));
    assert!(check_chat_name("a\u{7}b").unwrap_err().contains("control"));
    assert!(check_chat_name("a\nb").unwrap_err().contains("control"));
    assert!(check_chat_name("a\"b").unwrap_err().contains("quote"));
    assert!(check_chat_name("a\\b").unwrap_err().contains("backslash"));
    assert!(check_chat_name(&"n".repeat(MAX_NAME)).is_ok());
    assert!(check_chat_name(&"n".repeat(MAX_NAME + 1)).unwrap_err().contains("longer than"));
    assert!(check_chat_name(&"é".repeat(MAX_NAME / 2 + 1)).is_err(), "the bound counts bytes, as the daemon does");
}

#[test]
fn the_name_goes_in_without_touching_another_byte() {
    // No [chat] table: one is added after the rest, which stays byte for byte.
    let added = with_chat_name(FILE, "Ada");
    assert_eq!(added, format!("{FILE}\n[chat]\nname = \"Ada\"\n"));
    // A header with no key: the key goes under it.
    let bare = "[trust]\nroot_prefix = \"/w\"\n[chat]\n# who\n[display]\nfullscreen_vsync_pin = true\n";
    assert_eq!(with_chat_name(bare, "Ada"), bare.replace("[chat]\n", "[chat]\nname = \"Ada\"\n"));
    // A header that is the last line with no newline.
    assert_eq!(with_chat_name("[chat]", "Ada"), "[chat]\nname = \"Ada\"\n");
    // An existing line is replaced in place; the other table's `name` and its neighbors stay.
    let old = "[x]\nname = \"keep\"\n[chat]\nname = Bob\r\nother = 1\n";
    assert_eq!(with_chat_name(old, "Ada"), "[x]\nname = \"keep\"\n[chat]\nname = \"Ada\"\r\nother = 1\n");
    // A comment on the replaced line stays.
    assert_eq!(with_chat_name("[chat]\nname = \"Bob\"  # me\r\n", "Ada"), "[chat]\nname = \"Ada\"  # me\r\n");
    assert_eq!(with_chat_name("", "Ada"), "[chat]\nname = \"Ada\"\n");
    assert_eq!(with_chat_name("a = 1", "Ada"), "a = 1\n\n[chat]\nname = \"Ada\"\n");
    // The window's reader sees the name (the line is a quoted string with nothing to escape, as the daemon's strict reader wants).
    let mut s = Settings::default();
    s.merge_text(&added);
    assert_eq!(s.chat_name.as_deref(), Some("Ada"));
}

#[test]
fn saving_writes_the_loaded_file_or_makes_the_per_user_one() {
    let t = scratch("save");
    // The file the window loaded is the one written; the config folder is not touched.
    let loaded = t.join("loaded.toml");
    std::fs::write(&loaded, FILE).unwrap();
    let mut s = Settings { source: Some(loaded.clone()), ..Settings::default() };
    let env = env_in(&t.join("cfg"));
    assert_eq!(save_chat_name(&mut s, " Ada ", &env), Ok(loaded.clone()));
    assert_eq!(std::fs::read_to_string(&loaded).unwrap(), format!("{FILE}\n[chat]\nname = \"Ada\"\n"));
    assert_eq!(s.chat_name.as_deref(), Some("Ada"), "used at once");
    assert!(!t.join("cfg").exists());
    save_chat_name(&mut s, "Bob", &env).unwrap();
    assert_eq!(std::fs::read_to_string(&loaded).unwrap(), format!("{FILE}\n[chat]\nname = \"Bob\"\n"), "replaced in place");

    // No file loaded: the per-user file and its folder are made.
    let mut s = Settings::default();
    let path = save_chat_name(&mut s, "Ada", &env).unwrap();
    assert_eq!(path, sot_config_dir_in(&env).unwrap().join("settings.toml"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[chat]\nname = \"Ada\"\n");
    // The next start reads the same file.
    let mut next = Settings::default();
    next.merge_text(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(next.chat_name.as_deref(), Some("Ada"));
}

#[test]
fn a_refused_name_or_failed_write_changes_nothing() {
    let t = scratch("fail");
    let env = env_in(&t.join("cfg"));
    let mut s = Settings::default();
    assert!(save_chat_name(&mut s, "", &env).is_err());
    assert!(!t.join("cfg").exists(), "a refused name writes nothing");

    // A file that is not UTF-8 is left as it was.
    let bad = t.join("bad.toml");
    std::fs::write(&bad, [0xff, 0xfe, b'\n']).unwrap();
    let mut s = Settings { source: Some(bad.clone()), ..Settings::default() };
    let words = save_chat_name(&mut s, "Ada", &env).unwrap_err();
    assert!(words.contains("not saved") && words.contains("UTF-8"), "{words}");
    assert_eq!(std::fs::read(&bad).unwrap(), [0xff, 0xfe, b'\n']);
    assert_eq!(s.chat_name, None);

    // A folder that cannot be made says why.
    let file = t.join("plain");
    std::fs::write(&file, "x").unwrap();
    let mut s = Settings { source: Some(file.join("sub").join("settings.toml")), ..Settings::default() };
    assert!(save_chat_name(&mut s, "Ada", &env).unwrap_err().contains("not saved"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x");
    assert_eq!(s.chat_name, None);
}
