use super::*;

fn sample(pinned: bool) -> Record {
    Record {
        prefix: "/h/.local/share/sot".into(),
        config: "/h/.config/sot".into(),
        service: "systemd".into(),
        version: "0.6.7".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        installed_at: "2026-10-20T00:00:00Z".into(),
        hub: "hub".into(),
        daemon: true,
        frontend: false,
        pinned,
    }
}

/// The record's old shape: one key per line in schema 1's order, the pin only when pinned, read back by every reader
/// (this module's, and `InstallManifest`).
#[test]
fn the_record_keeps_the_shape_earlier_readers_take() {
    let text = text(&sample(false));
    let keys: Vec<&str> = text.lines().filter_map(|l| l.trim().strip_prefix('"')?.split('"').next()).collect();
    assert_eq!(keys, ["schema", "prefix", "config", "service", "version", "tag", "commit", "installed_at", "hub", "daemon", "frontend"]);
    assert_eq!(string(&text, "tag").as_deref(), Some("v0.6.7"));
    assert_eq!((boolean(&text, "daemon"), boolean(&text, "frontend"), boolean(&text, "pinned")), (Some(true), Some(false), None));
    let m: sot_updater::InstallManifest = serde_json::from_str(&text).unwrap();
    assert_eq!((m.version.as_deref(), m.pinned), (Some("0.6.7"), None));
    let pinned = text_of_pinned();
    assert!(pinned.contains(",\n  \"pinned\": true\n}"), "{pinned}");
    let m: sot_updater::InstallManifest = serde_json::from_str(&pinned).unwrap();
    assert_eq!(m.pinned, Some(true));
}

fn text_of_pinned() -> String {
    text(&sample(true))
}

/// The pin is an edit: setting and removing it changes no other byte, and a Windows record (another key order, CRLF)
/// takes it the same way.
#[test]
fn the_pin_is_an_edit_that_keeps_every_other_byte() {
    let plain = text(&sample(false));
    assert_eq!(with_pin(&plain, true), text_of_pinned());
    assert_eq!(with_pin(&text_of_pinned(), false), plain);
    assert_eq!(with_pin(&plain, false), plain);
    let windows = "{\r\n  \"schema\": 1,\r\n  \"hub\": \"\",\r\n  \"version\": \"0.6.6\",\r\n  \"pinned\": false\r\n}\r\n";
    let edited = with_pin(windows, true);
    assert_eq!(boolean(&edited, "pinned"), Some(true));
    assert_eq!(edited.matches("pinned").count(), 1, "{edited}");
    assert_eq!(string(&edited, "version").as_deref(), Some("0.6.6"));
}

/// Written whole: the old record or the new one, never a temporary file left beside it.
#[test]
fn the_record_is_written_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("install.json");
    std::fs::write(&path, "old").unwrap();
    write_whole(&path, "new").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    assert!(set_pin(&dir.path().join("none.json"), dir.path(), true).unwrap());
    assert_eq!(boolean(&std::fs::read_to_string(dir.path().join("none.json")).unwrap(), "pinned"), Some(true));
}

#[test]
fn the_record_time_is_utc_in_its_shape() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(20_746), (2026, 10, 20));
    let now = now_utc();
    assert_eq!((now.len(), &now[10..11], &now[19..]), (20, "T", "Z"), "{now}");
}

/// A repair run moves nothing: its other steps lay again what is there, so the Julia environments' own manifests are
/// kept (an install's are resolved again), and a readable record of the same build and role gets its pin and no other
/// change; a record cut short, one of another build, or a run that changes the role or the hub is written whole.
#[test]
fn a_repair_run_moves_nothing() {
    let before = text(&sample(false));
    let mut r = sample(true);
    r.installed_at = "2026-10-21T09:00:00Z".into();
    assert_eq!(text_for(&before, &r, true), with_pin(&before, true));
    assert_eq!(text_for(&before, &r, false), text(&r));
    assert_eq!(text_for("{ \"version\": \"0.6.7\", \"comm", &r, true), text(&r), "a cut record is written whole");
    let other = text(&Record { commit: "f".repeat(40), ..sample(false) });
    assert_eq!(text_for(&other, &r, true), text(&r), "another build is written whole");
    for changed in [Record { frontend: true, ..r.clone() }, Record { hub: "other-hub".into(), ..r.clone() }] {
        assert_eq!(text_for(&before, &changed, true), text(&changed), "a run that changes the role or the hub is no repair");
    }

    let dir = tempfile::tempdir().unwrap();
    let ran = sot_log::test_wait::run_within(std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()), std::time::Duration::from_secs(20));
    assert_eq!(ran.status.code(), Some(0), "{ran}");
    std::fs::create_dir_all(dir.path().join("julia/kernel")).unwrap();
    std::fs::write(dir.path().join("julia/kernel/Manifest.toml"), "resolved\n").unwrap();
    assert_eq!(super::super::julia::stale_manifests(dir.path(), false).len(), 1);
    assert!(super::super::julia::stale_manifests(dir.path(), true).is_empty());
}

