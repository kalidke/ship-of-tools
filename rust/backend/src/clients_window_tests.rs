use super::super::Clients;
use super::*;

fn connect(clients: &Clients, instance: Option<&str>) -> super::super::ClientGuard {
    let name = Some("fe@host-a".to_string());
    clients.register("c", "0.6.6", 1, "fe".into(), Some("host-a".into()), instance.map(Into::into), name)
}

/// Two connections of one window share its numbers, the second arriving after the first has left (a redial)
/// included; another window, or a connection of no window, has numbers of its own.
#[test]
fn a_windows_connections_share_its_numbers_and_no_one_else_does() {
    let clients = Clients::new();
    let first = connect(&clients, Some("w"));
    let n = first.numbers().number(Some(1));
    drop(first);
    let again = connect(&clients, Some("w"));
    assert_eq!(again.numbers().daemon_numbers(&[1]), vec![n], "the redial's numbers are the window's");
    for other in [connect(&clients, Some("v")), connect(&clients, None), connect(&clients, Some(""))] {
        assert_eq!(other.numbers().daemon_numbers(&[1]), vec![1], "not the window's");
    }
}

/// A window's connections own its pages by one owner, the second arriving after the first has left (a redial)
/// included, and a window away is listed as away until it comes back; another window (the same instance on another
/// host included), or a connection of no window, owns its pages by its own serial.
#[test]
fn a_windows_connections_share_the_owner_of_its_pages() {
    let clients = Clients::new();
    let first = connect(&clients, Some("w"));
    let owner = first.site_owner();
    drop(first);
    assert_eq!(clients.away_sites(), vec![owner], "the window is away");
    let again = connect(&clients, Some("w"));
    assert_eq!(again.site_owner(), owner, "the redial owns the window's pages");
    assert!(clients.away_sites().is_empty(), "a window with a live connection is not away");
    let name = Some("fe@host-b".to_string());
    let elsewhere = clients.register("c", "0.6.6", 1, "fe".into(), Some("host-b".into()), Some("w".into()), name);
    for other in [connect(&clients, Some("v")), connect(&clients, None), connect(&clients, Some("")), elsewhere] {
        assert_eq!(other.site_owner(), other.serial(), "not the window's");
    }
}

/// A redialed window starts as it was: used when a person last used it.
#[test]
fn a_redialed_window_keeps_its_presence() {
    let clients = Clients::new();
    let first = connect(&clients, Some("w"));
    clients.touch_person_input(first.serial());
    let used = clients.snapshot_with_active().clients[0].last_person_input_at;
    assert!(used.is_some());
    drop(first);
    let again = connect(&clients, Some("w"));
    let snap = clients.snapshot_with_active();
    assert_eq!(snap.clients[0].last_person_input_at, used);
    assert_eq!(snap.active().map(|a| a.serial), Some(again.serial()), "still the window in use");
    let alone = connect(&clients, None);
    let snap = clients.snapshot_with_active();
    let row = snap.clients.iter().find(|c| c.serial == alone.serial()).expect("its row");
    assert_eq!(row.last_person_input_at, None, "a connection of no window");
}

/// Of a window's two live connections (an old one not yet reaped and its redial), the newer gets what goes to the
/// window.
#[test]
fn of_a_windows_connections_the_newest_is_chosen() {
    let clients = Clients::new();
    let old = connect(&clients, Some("w"));
    clients.touch_person_input(old.serial());
    let new = connect(&clients, Some("w"));
    let snap = clients.snapshot_with_active();
    assert_eq!(snap.active().map(|a| a.serial), Some(new.serial()));
    assert_eq!(snap.in_use().map(|(to, why)| (to.serial, why)), Some((new.serial(), "last-used")));
}

/// The roster remembers at most `AWAY` windows with no live connection, forgetting the one away longest; a window
/// with a live connection is never forgotten.
#[test]
fn windows_away_are_bounded_and_a_live_window_is_kept() {
    let clients = Clients::new();
    let live = connect(&clients, Some("live"));
    let n = live.numbers().number(Some(1));
    let first_away = connect(&clients, Some("away-0"));
    first_away.numbers().number(Some(1));
    let first_owner = first_away.site_owner();
    drop(first_away);
    let mut owners = Vec::new();
    for i in 1..=AWAY {
        let away = connect(&clients, Some(&format!("away-{i}")));
        owners.push(away.site_owner());
        drop(away);
    }
    let windows = clients.inner.lock().unwrap().windows.len();
    assert_eq!(windows, AWAY + 1, "the live window and AWAY windows away");
    let away = clients.away_sites();
    assert_eq!(away, owners, "the windows away, the one away longest first");
    assert!(!away.contains(&first_owner), "the forgotten window is not away");
    assert!(!away.contains(&live.site_owner()), "the live window is not away");
    assert_eq!(connect(&clients, Some("away-0")).numbers().daemon_numbers(&[1]), vec![1], "the one away longest went");
    assert_eq!(connect(&clients, Some("live")).numbers().daemon_numbers(&[1]), vec![n], "the live window stays");
}
