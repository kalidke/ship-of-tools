// Wire tests for the session ops: hello, version.query, fe.presence and fe.command.

use super::*;

#[cfg(test)]
mod hello_version_tests {
    use super::{HelloReq, HelloRes};

    #[test]
    fn legacy_hello_req_defaults_to_preversioning() {
        // A pre-versioning frontend sends a HelloReq without the ADR 0030
        // `protocol` / `app_version` fields. `#[serde(default)]` must fill them
        // with the "pre-versioning peer" sentinels (0 / "") so the backend gate
        // can recognize the peer as legacy rather than failing the parse.
        let json = r#"{"client_id":"c1","last_seen_revision":7}"#;
        let req: HelloReq = serde_json::from_str(json).expect("legacy HelloReq deserializes");
        assert_eq!(req.client_id, "c1");
        assert_eq!(req.last_seen_revision, 7);
        assert_eq!(req.protocol, 0, "missing protocol → 0 (pre-versioning)");
        assert_eq!(req.app_version, "", "missing app_version → empty");
    }

    #[test]
    fn legacy_hello_res_defaults_to_preversioning() {
        // Symmetric guard for the response: a pre-versioning backend's HelloRes
        // lacks the two fields; the newer frontend must still deserialize it,
        // seeing protocol == 0 (which it warns about but tolerates).
        let json = r#"{"session_id":"s1","revision":3,"snapshot_pending":false}"#;
        let res: HelloRes = serde_json::from_str(json).expect("legacy HelloRes deserializes");
        assert_eq!(res.session_id, "s1");
        assert_eq!(res.protocol, 0, "missing protocol → 0 (legacy backend)");
        assert_eq!(res.app_version, "");
        // ADR 0035: a backend that predates the proxy omits the field — the
        // FE must read `false` (never arm proxy listeners), not fail.
        assert!(!res.proxy, "missing proxy → false (no proxy capability)");
    }

    #[test]
    fn proxy_connect_req_round_trips_and_token_is_optional() {
        // ADR 0035 handshake shapes. `token` absent on the wire must parse
        // (Unix-socket transport never sends one).
        let req: super::ProxyConnectReq =
            serde_json::from_str(r#"{"port":1241}"#).expect("tokenless req parses");
        assert_eq!(req.port, 1241);
        assert!(req.token.is_none());
        let back: super::ProxyConnectReq = serde_json::from_str(
            &serde_json::to_string(&super::ProxyConnectReq {
                port: 1236,
                token: Some("t".into()),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(back.port, 1236);
        assert_eq!(back.token.as_deref(), Some("t"));
    }

    #[test]
    fn lane_connect_req_round_trips_and_optionals_are_optional() {
        // ADR 0045 §2 handshake shapes. `voyage_id`/`token` absent on the
        // wire must parse (the supervisor lane and a tokenless Unix-socket
        // transport never send either).
        let req: super::LaneConnectReq =
            serde_json::from_str(r#"{"target":"ws-a","lane":"supervisor"}"#)
                .expect("minimal req parses");
        assert_eq!(req.target, "ws-a");
        assert_eq!(req.lane, "supervisor");
        assert!(req.voyage_id.is_none());
        assert!(req.token.is_none());
        let back: super::LaneConnectReq = serde_json::from_str(
            &serde_json::to_string(&super::LaneConnectReq {
                target: "ws-b".into(),
                lane: "voyage".into(),
                voyage_id: Some("v1".into()),
                token: Some("t".into()),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(back.target, "ws-b");
        assert_eq!(back.lane, "voyage");
        assert_eq!(back.voyage_id.as_deref(), Some("v1"));
        assert_eq!(back.token.as_deref(), Some("t"));
    }

    #[test]
    fn lane_connect_res_round_trips() {
        let res = super::LaneConnectRes { ok: true, pid: 4242, created: 99 };
        let back: super::LaneConnectRes =
            serde_json::from_str(&serde_json::to_string(&res).unwrap()).unwrap();
        assert!(back.ok);
        assert_eq!(back.pid, 4242);
        assert_eq!(back.created, 99);
    }

    #[test]
    fn versioned_hello_req_round_trips() {
        let req = HelloReq {
            client_id: "c2".into(),
            session_id: None,
            last_seen_revision: 0,
            token: None,
            protocol: 2,
            app_version: "0.2.0-dev+abc".into(),
            host: None,
            role: String::new(),
            instance: None,
            name: None,
            os_user: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: HelloReq = serde_json::from_str(&json).unwrap();
        assert_eq!(back.protocol, 2);
        assert_eq!(back.app_version, "0.2.0-dev+abc");
    }

    #[test]
    fn minimal_hello_req_defaults_identity_to_absent() {
        // A hello that declares no identity at all still deserializes:
        // `protocol` reads 0 and the hello gate refuses it by version,
        // never by a missing field.
        let json = r#"{"client_id":"c3","last_seen_revision":0}"#;
        let req: HelloReq = serde_json::from_str(json).expect("minimal HelloReq deserializes");
        assert_eq!(req.protocol, 0);
        assert_eq!(req.name, None);
        assert_eq!(req.host, None);
        assert_eq!(req.role, "");
    }

    #[test]
    fn hello_req_frontend_name_round_trips() {
        // Protocol 2: a frontend declares its address as `name`
        // (`fe@<host>`) — the one field every role uses; `fe_handle` is
        // gone from the wire.
        let req = HelloReq {
            client_id: "c4".into(),
            session_id: None,
            last_seen_revision: 0,
            token: None,
            protocol: 2,
            app_version: "0.2.0-dev+abc".into(),
            host: None,
            role: String::new(),
            instance: None,
            name: Some("fe@host-a".into()),
            os_user: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("fe_handle"));
        let back: HelloReq = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name.as_deref(), Some("fe@host-a"));
    }

    #[test]
    fn hello_req_declared_identity_round_trips() {
        // ADR 0046 decision 1: host/role/instance/name travel on every hello.
        let req = HelloReq {
            client_id: "c5".into(),
            session_id: None,
            last_seen_revision: 0,
            token: None,
            protocol: 2,
            app_version: "0.2.0-dev+abc".into(),
            host: Some("test-host".into()),
            role: "agent".into(),
            instance: Some("i1".into()),
            name: Some("test-host-agent".into()),
            os_user: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"name\":\"test-host-agent\""));
        let back: HelloReq = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name.as_deref(), Some("test-host-agent"));
        assert_eq!(back.host.as_deref(), Some("test-host"));
        assert_eq!(back.role, "agent");
        assert_eq!(back.instance.as_deref(), Some("i1"));
    }

    #[test]
    fn hello_os_user_is_absent_when_none_and_read_when_present() {
        let mut req: HelloReq = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
        assert_eq!(req.os_user, None);
        assert!(!serde_json::to_string(&req).unwrap().contains("os_user"));
        let with: HelloReq = serde_json::from_str(r#"{"client_id":"c","os_user":"uid:7"}"#).unwrap();
        assert_eq!(with.os_user.as_deref(), Some("uid:7"));
        req.os_user = Some("uid:7".into());
        assert!(serde_json::to_string(&req).unwrap().contains(r#""os_user":"uid:7""#));
    }

    #[test]
    fn this_process_declares_the_protocol_the_version_and_the_os_account() {
        let hello = HelloReq::this_process("c6", super::HANDOFF_ROLE, Some("host-a".into())).expect("the OS account is readable");
        assert_eq!(hello.client_id, "c6");
        assert_eq!(hello.protocol, crate::PROTOCOL_VERSION);
        assert_eq!(hello.app_version, crate::app_version());
        assert_eq!(hello.role, "handoff");
        assert_eq!(hello.host.as_deref(), Some("host-a"));
        assert_eq!(hello.os_user, sot_log::identity::os_account::own_account_id());
        assert!(hello.session_id.is_none() && hello.instance.is_none() && hello.name.is_none() && hello.token.is_none());
    }

    #[test]
    fn agent_join_req_round_trips() {
        let req = super::AgentJoinReq {
            workspace_id: "ws-1".into(),
            handle: "test-host-agent".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: super::AgentJoinReq = serde_json::from_str(&json).unwrap();
        assert_eq!(back.workspace_id, "ws-1");
        assert_eq!(back.handle, "test-host-agent");
    }
}

#[cfg(test)]
mod version_query_tests {
    use super::{ClientVersion, DaemonVersion, DisconnectedBox, VersionQueryRes};

    #[test]
    fn version_query_res_round_trips() {
        let res = VersionQueryRes {
            daemon: DaemonVersion {
                app_version: "0.6.0-dev+abc1234".into(),
                protocol: 1,
                lane_build: "abc1234def".into(),
                lane_proto: 1,
                host: "test-host".into(),
                hosts_toml_hash: "deadbeefcafef00d".into(),
                uptime_s: 10_800,
            },
            clients: vec![ClientVersion {
                client_id: "fe-1".into(),
                app_version: "0.6.0-dev+abc1234".into(),
                protocol: 1,
                host: Some("test-host".into()),
                role: "fe".into(),
                instance: Some("i1".into()),
                name: Some("fe@host-a".into()),
                active: true,
                sessions: None,
            }],
            disconnected: vec![DisconnectedBox {
                identity: "fe@host-b".into(),
                since_s: 720,
            }],
        };
        let json = serde_json::to_string(&res).unwrap();
        let back: VersionQueryRes = serde_json::from_str(&json).unwrap();
        assert_eq!(back.daemon.lane_build, "abc1234def");
        assert_eq!(back.daemon.lane_proto, 1);
        assert_eq!(back.daemon.host, "test-host");
        assert_eq!(back.daemon.hosts_toml_hash, "deadbeefcafef00d");
        assert_eq!(back.daemon.uptime_s, 10_800);
        assert_eq!(back.clients.len(), 1);
        assert_eq!(back.clients[0].client_id, "fe-1");
        assert_eq!(back.clients[0].name.as_deref(), Some("fe@host-a"));
        assert_eq!(back.clients[0].host.as_deref(), Some("test-host"));
        assert_eq!(back.clients[0].role, "fe");
        assert!(back.clients[0].active);
        assert_eq!(back.disconnected.len(), 1);
        assert_eq!(back.disconnected[0].identity, "fe@host-b");
        assert_eq!(back.disconnected[0].since_s, 720);
    }

    #[test]
    fn version_query_res_missing_disconnected_and_uptime_deserializes_to_defaults() {
        // A daemon that predates this feature omits `uptime_s` and
        // `disconnected` entirely — must still parse, reading 0 / empty
        // rather than failing the whole response.
        let json = serde_json::json!({
            "daemon": {
                "app_version": "0.6.0",
                "protocol": 1,
                "lane_build": "xyz",
            },
            "clients": [],
        });
        let back: VersionQueryRes = serde_json::from_value(json).expect("legacy peer parses");
        assert_eq!(back.daemon.uptime_s, 0);
        assert_eq!(back.disconnected, Vec::new());
    }

    #[test]
    fn client_version_without_active_frontend_fields_defaults_absent() {
        // A daemon that predates name/active omits them entirely —
        // must still deserialize, reading None/false rather than failing
        // the whole roster entry.
        let json = serde_json::json!({
            "client_id": "fe-1",
            "app_version": "0.6.0",
            "protocol": 1,
        });
        let cv: ClientVersion =
            serde_json::from_value(json).expect("legacy ClientVersion deserializes");
        assert_eq!(cv.name, None);
        assert_eq!(cv.host, None);
        assert_eq!(cv.role, "");
        assert!(!cv.active);
    }

    #[test]
    fn version_query_res_missing_clients_deserializes_to_empty() {
        // Mirrors `legacy_hello_res_defaults_to_preversioning`: a peer that
        // answers this op but omits the roster must still deserialize
        // rather than failing the whole response. Also omits `lane_proto`
        // (a daemon predating ADR 0045), `host` (topology plan §F step
        // 1), and `hosts_toml_hash` (topology plan §B, this lane) --
        // `#[serde(default)]` must supply 0 / "" rather than failing the
        // whole payload.
        let json = serde_json::json!({
            "daemon": { "app_version": "0.6.0", "protocol": 1, "lane_build": "abc" },
        });
        let res: VersionQueryRes = serde_json::from_value(json).expect("legacy payload deserializes");
        assert!(res.clients.is_empty());
        assert_eq!(res.daemon.app_version, "0.6.0");
        assert_eq!(res.daemon.lane_proto, 0);
        assert_eq!(res.daemon.host, "");
        assert_eq!(res.daemon.hosts_toml_hash, "");
    }
}

#[cfg(test)]
mod fe_presence_and_command_tests {
    use super::{
        ClientVersion, DeclaredSession, FeCommandEvt, FeCommandSendRes, FePresenceReq, FePresenceRes,
        FeSessionsReq, FeSessionsRes, PingReq, PingRes,
    };

    #[test]
    fn fe_presence_req_and_res_round_trip_empty() {
        let req = FePresenceReq {};
        assert_eq!(serde_json::to_value(&req).unwrap(), serde_json::json!({}));
        let _: FePresenceReq = serde_json::from_value(serde_json::json!({})).expect("empty req parses");

        let res = FePresenceRes { ok: true };
        let json = serde_json::to_string(&res).unwrap();
        let back: FePresenceRes = serde_json::from_str(&json).unwrap();
        assert!(back.ok);
    }

    #[test]
    fn ping_req_and_res_round_trip_empty() {
        // topology plan §F step 2: `ping` is an empty request, same wire
        // shape convention as `fe.presence` (a distinct op, never merged
        // with it — presence means "a person", ping means "the read half
        // is alive").
        let req = PingReq {};
        assert_eq!(serde_json::to_value(&req).unwrap(), serde_json::json!({}));
        let _: PingReq = serde_json::from_value(serde_json::json!({})).expect("empty req parses");

        let res = PingRes { ok: true };
        let json = serde_json::to_string(&res).unwrap();
        let back: PingRes = serde_json::from_str(&json).unwrap();
        assert!(back.ok);
    }

    #[test]
    fn fe_command_send_res_resolved_target_round_trips_and_defaults_absent() {
        // Design point E: a daemon that predates `resolved_target` omits it
        // — `sot-fe` must read `None`, the same value it reads for "no
        // active frontend", not fail the whole ack.
        let legacy: FeCommandSendRes =
            serde_json::from_value(serde_json::json!({ "ok": true })).expect("legacy ack parses");
        assert_eq!(legacy.resolved_target, None);
        assert_eq!(legacy.delivered_to, None, "a pre-field daemon says 'cannot say', not zero");

        let res = FeCommandSendRes {
            ok: true,
            resolved_target: Some("fe@host-a".into()),
            delivered_to: Some(1),
        };
        let json = serde_json::to_string(&res).unwrap();
        let back: FeCommandSendRes = serde_json::from_str(&json).unwrap();
        assert_eq!(back.resolved_target.as_deref(), Some("fe@host-a"));
        assert_eq!(back.delivered_to, Some(1));
    }

    #[test]
    fn fe_command_evt_target_serial_never_reaches_the_wire() {
        // Design point B: `target_serial` is a daemon-internal routing hint
        // riding in the SAME struct as the wire-serialized `FeCommandEvt`
        // (so `write_fe_command` can filter before writing) — it must never
        // actually appear in the JSON, in either direction.
        let evt = FeCommandEvt {
            v: 1,
            cmd: "notify".into(),
            args: serde_json::json!({"text": "hi"}),
            target: Some("fe@host-a".into()),
            target_serial: Some(42),
        };
        let json = serde_json::to_value(&evt).unwrap();
        assert!(
            json.get("target_serial").is_none(),
            "target_serial must never be serialized: {json}"
        );
        // And an incoming wire payload that somehow named it is ignored,
        // deserializing to None regardless.
        let mut with_stray_field = json.clone();
        with_stray_field["target_serial"] = serde_json::json!(99);
        let back: FeCommandEvt = serde_json::from_value(with_stray_field).unwrap();
        assert_eq!(back.target_serial, None);
    }

    #[test]
    fn fe_sessions_req_and_res_round_trip() {
        // Session-listing brief decision 2: the sending box's whole row
        // list rides in one field, and the bare ack mirrors `fe.presence`.
        let req = FeSessionsReq {
            sessions: vec![DeclaredSession {
                handle: "agent@host-a".into(),
                state: "working".into(),
                summary: "reading a brief".into(),
                status_at: "2026-09-28T00:00:00Z".into(),
            }],
        };
        let json = serde_json::to_value(&req).unwrap();
        let back: FeSessionsReq = serde_json::from_value(json).unwrap();
        assert_eq!(back.sessions.len(), 1);
        assert_eq!(back.sessions[0].handle, "agent@host-a");

        let res = FeSessionsRes { ok: true };
        let json = serde_json::to_string(&res).unwrap();
        let back: FeSessionsRes = serde_json::from_str(&json).unwrap();
        assert!(back.ok);
    }

    #[test]
    fn client_version_sessions_distinguishes_never_declared_from_declared_empty() {
        // An absent `sessions` key (a peer that predates the field, or one
        // that has never sent `fe.sessions`) must read as `None` —
        // "declares no sessions" — and stay distinguishable from a box
        // that HAS declared and currently has nothing to report
        // (`Some(vec![])`, "no sessions"). Collapsing the two into one
        // value is the exact confident-but-wrong failure this field
        // exists to close.
        let legacy_json = serde_json::json!({
            "client_id": "c1",
            "app_version": "0.6.0",
            "protocol": 1,
            "role": "fe",
        });
        let legacy: ClientVersion = serde_json::from_value(legacy_json).expect("legacy peer parses");
        assert_eq!(legacy.sessions, None, "no key on the wire means never-declared");

        let declared_empty = ClientVersion {
            client_id: "c2".into(),
            app_version: "0.6.0".into(),
            protocol: 1,
            host: None,
            role: "fe".into(),
            instance: None,
            name: Some("fe@host-a".into()),
            active: false,
            sessions: Some(vec![]),
        };
        let json = serde_json::to_value(&declared_empty).unwrap();
        assert_eq!(
            json.get("sessions"),
            Some(&serde_json::json!([])),
            "an empty declaration still serializes the key, unlike a never-declared peer"
        );
        let back: ClientVersion = serde_json::from_value(json).unwrap();
        assert_eq!(back.sessions, Some(vec![]), "declared-empty round-trips as Some([]), never None");
    }
}
