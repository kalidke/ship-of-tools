use super::*;

/// A fresh empty folder under the temporary one, removed when the test ends.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let d = std::env::temp_dir().join(format!("sot-chat-name-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Scratch(d)
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

#[test]
fn a_name_in_a_later_table_is_not_the_chats() {
    let old = "[chat]\nother = 1\n[x]\nname = \"keep\"\n";
    assert_eq!(with_chat_name(old, "Ada"), "[chat]\nname = \"Ada\"\nother = 1\n[x]\nname = \"keep\"\n");
}

#[test]
fn a_file_the_strict_reader_cannot_read_after_the_edit_is_left_alone() {
    let t = scratch("strict");
    let env = env_in(&t.join("cfg"));
    // Each is valid TOML or near it that the line scan does not read as `[chat]`, or that already names it twice.
    for (n, old) in [
        "[\"chat\"]\nname = \"Bob\"\n",
        "chat.name = \"Bob\"\n",
        "chat = { name = \"Bob\" }\n",
        "[[chat]]\nname = \"Bob\"\n",
        "[chat]\n\"name\" = \"Bob\"\n",
        "[chat]\nname = \"\"\"a\nb\"\"\"\n",
        "[chat]\nname = \"A\"\nname = \"B\"\n",
        "[chat]\nname = \"A\"\n[chat]\nname = \"B\"\n",
        "[trust\nroot_prefix = 1\n",
    ]
    .iter()
    .enumerate()
    {
        let f = t.join(format!("s{n}.toml"));
        std::fs::write(&f, old).unwrap();
        let mut s = Settings { source: Some(f.clone()), ..Settings::default() };
        let words = save_chat_name(&mut s, "Ada", &env).unwrap_err();
        assert!(words.contains("will not edit") && words.contains("name = \"Ada\""), "{old:?}: {words}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), *old, "{old:?}");
        assert_eq!(s.chat_name, None);
    }
    // A leading byte order mark is kept; the strict reader reads it, so the name goes in.
    let f = t.join("bom.toml");
    let old = "\u{feff}[trust]\nroot_prefix = \"/w\"\n";
    std::fs::write(&f, old).unwrap();
    let mut s = Settings { source: Some(f.clone()), ..Settings::default() };
    save_chat_name(&mut s, "Ada", &env).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), format!("{old}\n[chat]\nname = \"Ada\"\n"));
    let g = t.join("bom2.toml");
    std::fs::write(&g, "\u{feff}[chat]\nname = \"Bob\"\n").unwrap();
    let mut s = Settings { source: Some(g.clone()), ..Settings::default() };
    save_chat_name(&mut s, "Ada", &env).unwrap();
    assert_eq!(std::fs::read_to_string(&g).unwrap(), "\u{feff}[chat]\nname = \"Ada\"\n");
    // The window's reader reads it back through the mark.
    let mut read = Settings::default();
    read.merge_text(&std::fs::read_to_string(&g).unwrap());
    assert_eq!(read.chat_name.as_deref(), Some("Ada"));
}

#[test]
fn a_read_error_other_than_not_found_writes_nothing() {
    let t = scratch("readerr");
    let env = env_in(&t.join("cfg"));
    let dir = t.join("settings.toml");
    std::fs::create_dir(&dir).unwrap();
    let mut s = Settings { source: Some(dir.clone()), ..Settings::default() };
    assert!(save_chat_name(&mut s, "Ada", &env).unwrap_err().contains("not saved"));
    assert!(dir.is_dir(), "the path is as it was");
    assert_eq!(s.chat_name, None);
}

#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_stays_a_link() {
    let t = scratch("link");
    let env = env_in(&t.join("cfg"));
    let real = t.join("dotfiles").join("real.toml");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, FILE).unwrap();
    let link = t.join("settings.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut s = Settings { source: Some(link.clone()), ..Settings::default() };
    save_chat_name(&mut s, "Ada", &env).unwrap();
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink(), "the link is not replaced");
    assert_eq!(std::fs::read_to_string(&real).unwrap(), format!("{FILE}\n[chat]\nname = \"Ada\"\n"));
}

/// A file that cannot be read is not an empty one: writing over it would lose what it holds.
#[cfg(unix)]
#[test]
fn an_unreadable_file_is_left_as_it_is() {
    use std::os::unix::fs::PermissionsExt;
    let t = scratch("unreadable");
    let env = env_in(&t.join("cfg"));
    let f = t.join("settings.toml");
    std::fs::write(&f, FILE).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&f).is_ok() {
        return; // a reader that ignores the mode (root) cannot show it
    }
    let mut s = Settings { source: Some(f.clone()), ..Settings::default() };
    assert!(save_chat_name(&mut s, "Ada", &env).unwrap_err().contains("not saved"));
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), FILE);
    assert_eq!(s.chat_name, None);
}
