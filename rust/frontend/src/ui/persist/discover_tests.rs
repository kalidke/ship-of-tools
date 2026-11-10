use super::*;

/// A fresh empty folder under the temporary one, named for its test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sot-discover-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn put(dir: &Path, rel: &str) -> PathBuf {
    let f = dir.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "").unwrap();
    f
}

#[cfg(unix)]
#[test]
fn xdg_config_home_wins_over_home() {
    let t = scratch("xdg_config_h");
    let xdg = put(&t.join("xdg"), "sot/settings.toml");
    put(&t.join("home"), ".config/sot/settings.toml");
    let env = Env::empty().with("XDG_CONFIG_HOME", &t.join("xdg")).with("HOME", &t.join("home"));
    assert_eq!(find_config_file_in(&env, None, "SOT_SETTINGS", "settings.toml"), Some(xdg));
}

/// Windows ignores `XDG_CONFIG_HOME` and `HOME`: the folder is under `LOCALAPPDATA`.
#[cfg(windows)]
#[test]
fn localappdata_names_the_config_folder_and_home_is_ignored() {
    let t = scratch("winbase");
    let f = put(&t.join("local"), "sot/config/settings.toml");
    put(&t.join("home"), ".config/sot/settings.toml");
    let env = Env::empty().with("LOCALAPPDATA", t.join("local")).with("HOME", t.join("home"));
    assert_eq!(find_config_file_in(&env, None, "SOT_SETTINGS", "settings.toml"), Some(f));
}

#[test]
fn a_config_base_alone_finds_the_file_without_home() {
    let t = scratch("a_config_bas");
    let env = if cfg!(windows) {
        Env::empty().with("LOCALAPPDATA", &t)
    } else {
        Env::empty().with("XDG_CONFIG_HOME", &t)
    };
    let dir = sot_config_dir_in(&env).unwrap();
    let f = put(&dir, "keybindings.toml");
    assert_eq!(find_config_file_in(&env, None, "SOT_KEYBINDINGS", "keybindings.toml"), Some(f));
}

#[test]
fn the_variable_then_the_repo_walk_come_before_the_per_user_file() {
    let t = scratch("the_variable");
    let base = t.join("base");
    let env = if cfg!(windows) {
        Env::empty().with("LOCALAPPDATA", &base)
    } else {
        Env::empty().with("XDG_CONFIG_HOME", &base)
    };
    put(&sot_config_dir_in(&env).unwrap(), "settings.toml");
    let repo = t.join("repo");
    let up = put(&repo, ".sot/settings.toml");
    let cwd = repo.join("a/b");
    std::fs::create_dir_all(&cwd).unwrap();
    assert_eq!(find_config_file_in(&env, Some(&cwd), "SOT_SETTINGS", "settings.toml"), Some(up));
    let own = put(&cwd, ".sot/settings.toml");
    assert_eq!(find_config_file_in(&env, Some(&cwd), "SOT_SETTINGS", "settings.toml"), Some(own));
    let named = put(&t, "named.toml");
    let env = env.with("SOT_SETTINGS", &named);
    assert_eq!(find_config_file_in(&env, Some(&cwd), "SOT_SETTINGS", "settings.toml"), Some(named));
}
