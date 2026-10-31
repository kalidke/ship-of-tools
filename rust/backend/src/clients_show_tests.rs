use super::*;
use crate::clients::{ClientInfo, Clients};
use std::time::{Duration, Instant};

fn fe(serial: u64, name: &str, used: Option<Instant>) -> ClientInfo {
    ClientInfo {
        serial,
        client_id: format!("c{serial}"),
        app_version: "0.6.6".into(),
        protocol: 1,
        host: None,
        role: "fe".into(),
        instance: None,
        name: Some(name.into()),
        last_person_input_at: used,
        sessions: None,
    }
}

fn chosen(clients: &[ClientInfo]) -> Option<(String, &'static str)> {
    let snapshot = ClientsSnapshot { clients: clients.to_vec(), active: None, away: vec![] };
    snapshot.in_use().ok().map(|(to, why)| (to.handle, why))
}

fn pick(handle: &str, why: &'static str) -> Option<(String, &'static str)> {
    Some((handle.to_string(), why))
}

#[test]
fn the_window_in_use_is_the_one_with_the_latest_input_the_one_marked_active() {
    let now = Instant::now() + Duration::from_secs(3600);
    let s = |secs| Some(now - Duration::from_secs(secs));
    let clients = [fe(1, "fe@a", s(180)), fe(2, "fe@b", s(5)), fe(3, "fe@c", None)];
    assert_eq!(chosen(&clients), pick("fe@b", "last-used"), "the latest input, not the window connected last");
    let active = Clients::resolve_active(&clients, now).map(|a| a.handle);
    assert_eq!(active.as_deref(), Some("fe@b"), "the window `sot-fe version` marks active");
}

#[test]
fn with_no_window_active_the_one_used_last_and_with_none_used_the_one_connected_last() {
    let now = Instant::now() + Duration::from_secs(3600);
    let s = |secs| Some(now - Duration::from_secs(secs));
    let idle = [fe(1, "fe@a", s(1200)), fe(2, "fe@b", s(600)), fe(3, "fe@c", None)];
    assert_eq!(Clients::resolve_active(&idle, now), None, "no window used within five minutes");
    assert_eq!(chosen(&idle), pick("fe@b", "last-used"), "the window used last, however long ago");
    let never = [fe(1, "fe@a", None), fe(2, "fe@b", None)];
    assert_eq!(chosen(&never), pick("fe@b", "last-connected"), "no window ever used");
    let mut others = [fe(1, "fe@a", None), fe(2, "", None), fe(3, "fe@c", None)];
    others[2].role = "agent".into();
    assert_eq!(chosen(&others), pick("fe@a", "last-connected"), "only named frontends are chosen");
    others[0].name = None;
    assert_eq!(chosen(&others), None, "no window attached: none");
}

#[test]
fn a_preview_goes_to_every_window_and_every_other_command_to_one() {
    let clients = Clients::new();
    let a = clients.register("a", "0.6.6", 1, "fe".into(), None, None, Some("fe@a".into()));
    let _b = clients.register("b", "0.6.6", 1, "fe".into(), None, None, Some("fe@b".into()));
    clients.touch_person_input(a.serial());
    let snap = clients.snapshot_with_active();
    for cmd in ["preview", "reveal"] {
        assert_eq!(snap.window_for(cmd), Route::Every, "{cmd}: every window");
    }
    for cmd in ["docs", "open_url", "notify", "goto_workspace", "goto_mode", "relaunch"] {
        let to = match snap.window_for(cmd) {
            Route::One(to, _) => Some(to.serial),
            _ => None,
        };
        assert_eq!(to, Some(a.serial()), "{cmd}: the window in use");
    }
}

fn window(clients: &Clients, host: &str, instance: &str) -> crate::clients::ClientGuard {
    let name = Some(format!("fe@{host}"));
    clients.register("c", "0.7.0", 4, "fe".into(), Some(host.into()), Some(instance.into()), name)
}

fn to(guard: &crate::clients::ClientGuard, host: &str, why: &'static str) -> Route {
    Route::One(ActiveFrontend { serial: guard.serial(), handle: format!("fe@{host}") }, why)
}

/// The window a person used last is away (a laptop asleep or closed): a browser open goes to no other window, and the
/// route names its computer; a live window on that computer (a relaunched window) takes it; once the person uses
/// another window, that window does. Seen in use 2026-10-31: pages piled up on a computer the maintainer had left,
/// sent there while the window he had used last was away.
#[test]
fn a_window_used_last_that_is_away_sends_nothing_to_another_window() {
    let clients = Clients::new();
    let t0 = Instant::now();
    let desk = window(&clients, "desk", "b");
    let laptop = window(&clients, "laptop", "l");
    clients.inner.lock().unwrap().report(desk.serial(), t0);
    clients.inner.lock().unwrap().report(laptop.serial(), t0 + Duration::from_secs(60));
    assert_eq!(clients.snapshot_with_active().window_for("docs"), to(&laptop, "laptop", "last-used"));
    drop(laptop);
    let snap = clients.snapshot_with_active();
    for cmd in ["docs", "open_url", "notify"] {
        assert_eq!(snap.window_for(cmd), Route::Nowhere(Some("laptop".into())), "{cmd}: not the computer left earlier");
    }
    assert_eq!(snap.window_for("preview"), Route::Every, "a preview still reaches every window");
    assert_eq!(snap.in_use(), Err(Some("laptop".into())));

    let relaunched = window(&clients, "laptop", "l2");
    assert_eq!(clients.snapshot_with_active().window_for("docs"), to(&relaunched, "laptop", "same-computer"));
    drop(relaunched);

    clients.inner.lock().unwrap().report(desk.serial(), t0 + Duration::from_secs(120));
    assert_eq!(clients.snapshot_with_active().window_for("docs"), to(&desk, "desk", "last-used"), "used after");
}
