//! Unit tests for the client roster (clients.rs).
    use super::*;

    #[test]
    fn register_and_drop_track_count() {
        let clients = Clients::new();
        assert_eq!(clients.count(), 0);

        let g1 = clients.register("client-a", "tcp", Some("127.0.0.1:5000".into()), "0.6.0", 1, String::new(), None, None, None);
        assert_eq!(clients.count(), 1);

        let g2 = clients.register("client-b", "local", None, "0.6.0", 1, String::new(), None, None, None);
        assert_eq!(clients.count(), 2);
        assert_eq!(clients.snapshot_with_active().clients.len(), 2);

        drop(g1);
        assert_eq!(clients.count(), 1);
        drop(g2);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn same_client_id_two_connections_are_distinct() {
        let clients = Clients::new();
        let g1 = clients.register("client-a", "tcp", None, "0.6.0", 1, String::new(), None, None, None);
        let g2 = clients.register("client-a", "tcp", None, "0.6.0", 1, String::new(), None, None, None);
        // Two live connections, one distinct client.
        assert_eq!(clients.count(), 2);
        assert_eq!(distinct_client_ids(&clients.inner.lock().unwrap().by_conn), "client-a");
        drop(g1);
        assert_eq!(clients.count(), 1);
        drop(g2);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn snapshot_carries_app_version_protocol_and_own_serial() {
        // Decision 31b: this is exactly what `version.query`'s `clients[]`
        // roster reads — sourced from registration, not a new probe.
        let clients = Clients::new();
        let g = clients.register("client-a", "tcp", None, "0.6.0-dev+abc1234", 1, String::new(), None, None, None);
        let snap = clients.snapshot_with_active();
        assert_eq!(snap.clients.len(), 1);
        assert_eq!(snap.clients[0].app_version, "0.6.0-dev+abc1234");
        assert_eq!(snap.clients[0].protocol, 1);
        assert_eq!(snap.clients[0].serial, g.serial());
    }

    #[test]
    fn touch_person_input_stamps_only_the_named_serial() {
        let clients = Clients::new();
        let g1 = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        let _g2 = clients.register("client-b", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-b".into()));

        assert!(clients
            .snapshot_with_active()
            .clients
            .iter()
            .all(|c| c.last_person_input_at.is_none()));

        clients.touch_person_input(g1.serial());
        let snap = clients.snapshot_with_active();
        let a = snap.clients.iter().find(|c| c.client_id == "client-a").unwrap();
        let b = snap.clients.iter().find(|c| c.client_id == "client-b").unwrap();
        assert!(a.last_person_input_at.is_some(), "the touched connection is stamped");
        assert!(b.last_person_input_at.is_none(), "an untouched connection stays unstamped");
    }

    #[test]
    fn touch_person_input_on_a_departed_serial_is_a_harmless_noop() {
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        let serial = g.serial();
        drop(g);
        // Must not panic on a serial that no longer has an entry.
        clients.touch_person_input(serial);
        assert_eq!(clients.count(), 0);
    }

    fn declared_session(handle: &str, state: &str) -> sot_protocol::DeclaredSession {
        sot_protocol::DeclaredSession {
            handle: handle.to_string(),
            state: state.to_string(),
            summary: String::new(),
            status_at: String::new(),
        }
    }

    #[test]
    fn declare_sessions_lands_on_its_serial_and_dies_with_the_connection() {
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        assert!(
            clients.snapshot_with_active().clients[0].sessions.is_none(),
            "never-declared reads as None before the first fe.sessions"
        );

        clients.declare_sessions(g.serial(), vec![declared_session("agent@host-a", "working")]);
        let snap = clients.snapshot_with_active();
        let sessions = snap.clients[0].sessions.as_ref().expect("declared once, so Some");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].handle, "agent@host-a");
        assert_eq!(sessions[0].state, "working");

        drop(g);
        assert_eq!(clients.count(), 0, "the declaration is gone the moment the connection is");
    }

    #[test]
    fn declare_sessions_on_a_departed_serial_is_a_harmless_noop() {
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        let serial = g.serial();
        drop(g);
        // Must not panic on a serial that no longer has an entry.
        clients.declare_sessions(serial, vec![declared_session("agent@host-a", "working")]);
        assert_eq!(clients.count(), 0);
    }

    #[test]
    fn closing_one_session_removes_only_that_handle_others_keep_their_state() {
        // Amendment 1: the acceptance check is closing ONE session, not
        // dropping a connection. Declaration is edge-driven — the box
        // re-declares its whole row list whenever it changes — so closing
        // one row on a box with several must drop only that handle from
        // the NEXT declaration, leaving the others' state untouched.
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        clients.declare_sessions(
            g.serial(),
            vec![
                declared_session("agent-1@host-a", "working"),
                declared_session("agent-2@host-a", "idle"),
                declared_session("agent-3@host-a", "blocked"),
            ],
        );

        // agent-2's row closes; the box re-declares its now-shorter list.
        clients.declare_sessions(
            g.serial(),
            vec![
                declared_session("agent-1@host-a", "working"),
                declared_session("agent-3@host-a", "blocked"),
            ],
        );

        let snap = clients.snapshot_with_active();
        let sessions = snap.clients[0].sessions.as_ref().expect("still declared");
        let handles: Vec<&str> = sessions.iter().map(|s| s.handle.as_str()).collect();
        assert_eq!(
            handles,
            vec!["agent-1@host-a", "agent-3@host-a"],
            "only the closed handle leaves the list"
        );
        assert_eq!(sessions[0].state, "working", "agent-1's state survives untouched");
        assert_eq!(sessions[1].state, "blocked", "agent-3's state survives untouched");
    }

    #[test]
    fn a_box_that_had_declared_appears_disconnected_once_its_connection_drops() {
        // Session-listing brief, amendment 2's final (no-heartbeat) form:
        // the sessions leave with the connection, but the BOX is retained
        // as one "not connected since" entry — never silently absent.
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        clients.declare_sessions(g.serial(), vec![declared_session("agent@host-a", "working")]);
        drop(g);

        let disconnected = clients.disconnected_since(Instant::now());
        assert_eq!(disconnected.len(), 1);
        assert_eq!(disconnected[0].0, "fe@host-a");
    }

    #[test]
    fn a_connection_that_never_declared_leaves_no_disconnected_entry() {
        // A bridge/cli/agent connection (or an old frontend) that never
        // sent fe.sessions has nothing to be missed FOR — dropping it
        // must not manufacture a "not connected" line for a box that
        // never claimed to have sessions in the first place.
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        drop(g);

        assert_eq!(
            clients.disconnected_since(Instant::now()),
            Vec::new(),
            "no fe.sessions was ever sent, so no box should be retained as disconnected"
        );
    }

    #[test]
    fn a_later_declaration_from_the_same_identity_clears_its_disconnected_entry() {
        // A box that just declared again is, by definition, not missing:
        // a FRESH connection under the same name clears the entry the
        // PREVIOUS connection's drop left behind.
        let clients = Clients::new();
        let g1 = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        clients.declare_sessions(g1.serial(), vec![declared_session("agent@host-a", "working")]);
        drop(g1);
        assert_eq!(clients.disconnected_since(Instant::now()).len(), 1, "disconnected after the first drop");

        let g2 = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        clients.declare_sessions(g2.serial(), vec![declared_session("agent@host-a", "working")]);
        assert_eq!(
            clients.disconnected_since(Instant::now()),
            Vec::new(),
            "re-declaring clears the earlier disconnected entry"
        );
    }

    #[test]
    fn a_second_declaration_replaces_the_first_rather_than_accumulating() {
        let clients = Clients::new();
        let g = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-a".into()));
        clients.declare_sessions(g.serial(), vec![declared_session("agent-1@host-a", "working")]);
        clients.declare_sessions(g.serial(), vec![declared_session("agent-2@host-a", "idle")]);

        let snap = clients.snapshot_with_active();
        let sessions = snap.clients[0].sessions.as_ref().expect("declared");
        assert_eq!(sessions.len(), 1, "the second call replaces the list, it does not append to it");
        assert_eq!(sessions[0].handle, "agent-2@host-a");
    }

    #[test]
    fn active_frontend_requires_both_a_handle_and_a_fresh_stamp() {
        let clients = Clients::new();
        // No handle at all: never active, even though it's touched.
        let no_handle = clients.register("client-a", "local", None, "0.6.0", 1, String::new(), None, None, None);
        clients.touch_person_input(no_handle.serial());
        assert_eq!(
            clients.snapshot_with_active().active(),
            None,
            "a client with no name is never active"
        );

        // A handle but never touched: not active either.
        let untouched = clients.register("client-b", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-b".into()));
        let _ = &untouched;
        assert_eq!(clients.snapshot_with_active().active(), None);
    }

    #[test]
    fn active_excludes_a_named_connection_whose_role_is_not_fe() {
        // `name` alone never makes a connection a frontend: a cli/agent/
        // bridge's declared handle is not an address a command lands on.
        let clients = Clients::new();
        let g = clients.register(
            "client-a",
            "local",
            None,
            "0.6.0",
            1,
            "cli".to_string(),
            None,
            None,
            Some("not-really-a-frontend".into()),
        );
        clients.touch_person_input(g.serial());
        assert_eq!(
            clients.snapshot_with_active().active(),
            None,
            "a non-fe role is never active even with a name and a fresh touch"
        );
    }

    #[test]
    fn active_requires_the_fe_role_even_when_named_and_touched() {
        // An agent's declared `name` must never make its connection
        // selectable as the active frontend, touched or not.
        let clients = Clients::new();
        let g = clients.register(
            "client-a",
            "local",
            None,
            "0.6.0",
            1,
            "agent".to_string(),
            None,
            None,
            Some("test-host-agent".into()),
        );
        clients.touch_person_input(g.serial());
        assert_eq!(
            clients.snapshot_with_active().active(),
            None,
            "a declared `name` on a non-fe role is never active"
        );
    }

    /// A directly-constructed `ClientInfo` for `Clients::resolve_active`'s
    /// pure-function tests below — bypasses the registry lock and real
    /// `Instant::now()` entirely, so `now` and every stamp are exactly the
    /// values the test chose (2026-09-08 review, finding 8: "inject the
    /// clock; no sleeps").
    fn ci(serial: u64, handle: &str, last_person_input_at: Option<Instant>) -> ClientInfo {
        ClientInfo {
            serial,
            client_id: format!("client-{serial}"),
            transport: "local",
            peer: None,
            connected_at: 0,
            app_version: "0.6.0".into(),
            protocol: 1,
            role: "fe".to_string(),
            host: None,
            instance: None,
            name: Some(handle.to_string()),
            last_person_input_at,
            sessions: None,
        }
    }

    #[test]
    fn active_frontend_prefers_the_more_recent_stamp_no_sleeps() {
        // Deterministic ordering, one fixed `now`, no real waiting
        // (2026-09-08 review, finding 8: the prior version of this test
        // slept 1.1s to dodge a same-second tie).
        let now = Instant::now();
        let clients = vec![
            ci(1, "fe@host-a", Some(now - Duration::from_secs(10))),
            ci(2, "fe@host-b", Some(now - Duration::from_secs(1))),
        ];
        let active = Clients::resolve_active(&clients, now);
        assert_eq!(
            active,
            Some(ActiveFrontend { serial: 2, handle: "fe@host-b".to_string() }),
            "the more recently touched frontend wins, identified by its serial"
        );
    }

    #[test]
    fn active_frontend_equal_stamp_ties_go_to_the_later_registered_connection() {
        // "on equal stamps, the later-registered connection wins" —
        // registration order is `serial`, which `resolve_active`'s
        // `(instant, serial)` ordering uses as its tie-break.
        let tie = Instant::now();
        let clients = vec![ci(1, "fe@host-a", Some(tie)), ci(2, "fe@host-b", Some(tie))];
        let active = Clients::resolve_active(&clients, tie);
        assert_eq!(
            active,
            Some(ActiveFrontend { serial: 2, handle: "fe@host-b".to_string() }),
            "an exact tie goes to the later-registered (higher-serial) connection"
        );
    }

    #[test]
    fn active_frontend_window_boundary_exactly_in_then_one_past() {
        let now = Instant::now();
        let exactly_at_window = vec![ci(1, "fe@host-a", Some(now - ACTIVE_WINDOW))];
        assert!(
            Clients::resolve_active(&exactly_at_window, now).is_some(),
            "a stamp exactly ACTIVE_WINDOW old is still active (inclusive boundary)"
        );

        let one_past_window = vec![ci(1, "fe@host-a", Some(now - ACTIVE_WINDOW - Duration::from_millis(1)))];
        assert!(
            Clients::resolve_active(&one_past_window, now).is_none(),
            "one millisecond past the window no longer counts as \"a person is here\""
        );
    }

    #[test]
    fn duplicate_handle_active_resolves_to_one_serial_not_both_rows() {
        // Two connections sharing a handle (a stale reconnect, or a genuine
        // hostname collision) must resolve to exactly one winner BY SERIAL
        // — `is_active_serial` must be true for that one and false for the
        // other, never both (2026-09-08 review, finding 5).
        let clients = Clients::new();
        let a = clients.register("client-a", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-dup".into()));
        let b = clients.register("client-b", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@host-dup".into()));
        clients.touch_person_input(a.serial());

        let snap = clients.snapshot_with_active();
        assert_eq!(snap.active().map(|x| x.serial), Some(a.serial()));
        assert!(snap.is_active_serial(a.serial()));
        assert!(!snap.is_active_serial(b.serial()), "the untouched duplicate is never active");
    }

    #[test]
    fn receivers_for_matches_by_name_or_unconditionally_by_fe_role() {
        // Roster: a bridge named "X", a frontend named "fe@h" (no bridge,
        // per the module doc — an "fe" row always counts), and the
        // requester itself ("cli", never its own receiver).
        let clients = Clients::new();
        let bridge = clients.register("client-x", "local", None, "0.6.0", 1, "bridge".to_string(), None, None, Some("X".into()));
        let fe = clients.register("client-fe", "local", None, "0.6.0", 1, "fe".to_string(), None, None, Some("fe@h".into()));
        let me = clients.register("client-me", "local", None, "0.6.0", 1, "cli".to_string(), None, None, Some("self".into()));
        let _ = (&bridge, &fe);

        let mut to_x = clients.receivers_for("X", me.serial());
        to_x.sort();
        assert_eq!(to_x, vec!["X".to_string(), "fe@h".to_string()], "the named match plus the always-on fe row");

        let mut to_absent = clients.receivers_for("Y", me.serial());
        to_absent.sort();
        assert_eq!(to_absent, vec!["fe@h".to_string()], "no name matches, but fe still counts");

        let mut broadcast = clients.receivers_for("", me.serial());
        broadcast.sort();
        assert_eq!(broadcast, vec!["X".to_string(), "fe@h".to_string()], "broadcast is every OTHER named row");

        assert!(
            !clients.receivers_for("self", me.serial()).contains(&"self".to_string()),
            "self_serial is always excluded, even if `to` names the requester's own handle"
        );
    }
