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

#[test]
fn xdg_config_home_wins_over_home() {
    let t = scratch("xdg_config_h");
    let xdg = put(&t.join("xdg"), "sot/settings.toml");
    put(&t.join("home"), ".config/sot/settings.toml");
    let env = Env::empty().with("XDG_CONFIG_HOME", &t.join("xdg")).with("HOME", &t.join("home"));
    assert_eq!(find_config_file_in(&env, None, "SOT_SETTINGS", "settings.toml"), Some(xdg));
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
    let xdg = &t.join("xdg");
    put(&xdg, "sot/settings.toml");
    let env = Env::empty().with("XDG_CONFIG_HOME", &xdg);
    let repo = &t.join("repo");
    let local = put(&repo, ".sot/settings.toml");
    let cwd = repo.join("a/b");
    std::fs::create_dir_all(&cwd).unwrap();
    assert_eq!(find_config_file_in(&env, Some(&cwd), "SOT_SETTINGS", "settings.toml"), Some(local));
    let named = put(&t, "named.toml");
    let env = env.with("SOT_SETTINGS", &named);
    assert_eq!(find_config_file_in(&env, Some(&cwd), "SOT_SETTINGS", "settings.toml"), Some(named));
}
