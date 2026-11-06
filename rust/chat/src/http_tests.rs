//! Behavioral tests of `http` against the scripted fake Slack over real TLS (see `fake_slack`).

use std::future::Future;
use std::time::{Duration, Instant};

use crate::fake_slack::{eventually, FakeSlack, Identity, Reply};
use crate::http::{
    is_slack_host, Http, HttpError, Request, Secret, MAX_BODY, MAX_OPERATIONS, MAX_REDIRECTS,
};

const TOKEN: &str = "sentinel-bot-token-not-a-credential";
const PRIVATE: &str = "sentinel-private-file-path";
const SHORT: Duration = Duration::from_millis(400);
const LONG: Duration = Duration::from_secs(10);

/// Every test body runs under one bound: a hung fixture fails here instead of waiting.
async fn bounded<T>(body: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), body)
        .await
        .expect("the test exceeded its bound")
}

fn get(fake: &FakeSlack, host: &str, path: &str) -> Request {
    Request::get(fake.url(host, path), LONG)
}

fn count(fake: &FakeSlack, target: &str) -> usize {
    fake.seen().iter().filter(|s| s.target == target).count()
}

#[test]
fn slack_hosts_are_slack_com_and_its_dot_boundary_subdomains() {
    for yes in [
        "slack.com",
        "files.slack.com",
        "a.b.slack.com",
        "wss-primary.slack.com",
    ] {
        assert!(is_slack_host(yes), "{yes}");
    }
    for no in [
        "evilslack.com",
        "slack.com.evil.example",
        "slack.co",
        "xslack.com",
        ".slack.com",
        "a..slack.com",
        "slack.com.",
        "",
        "slack.comx",
    ] {
        assert!(!is_slack_host(no), "{no}");
    }
}

#[tokio::test]
async fn private_redirect_keeps_slack_auth_and_refuses_foreign_destinations() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, port| {
            let to = |host: &str| Reply::redirect(&format!("https://{host}:{port}/x"));
            match seen.target.as_str() {
                "/start-slack" => Reply::redirect(&format!("https://api.slack.com:{port}/final")),
                "/start-relative" => Reply::redirect("/final"),
                "/to-lookalike" => to("evilslack.com"),
                "/to-sub-lookalike" => to("slack.com.evil.example"),
                "/to-foreign" => to("other.example"),
                "/to-ip" => to("127.0.0.1"),
                "/to-downgrade" => Reply::redirect(&format!("http://files.slack.com:{port}/x")),
                "/to-userinfo" => {
                    Reply::redirect(&format!("https://user:word@files.slack.com:{port}/x"))
                }
                "/to-userinfo-trick" => {
                    Reply::redirect(&format!("https://files.slack.com:{port}@evilslack.com/x"))
                }
                "/to-port" => Reply::redirect("https://files.slack.com:1/x"),
                "/to-scheme-relative" => Reply::redirect(&format!("//evilslack.com:{port}/x")),
                "/to-malformed" => Reply::redirect("https://[not-an-address/x"),
                "/loop" => Reply::redirect("/loop"),
                "/final" => Reply::ok(b"final"),
                _ => Reply::ok(b"reached"),
            }
        })
        .await;
        let http = fake.http();
        let bearer = || Secret::new(TOKEN);
        let want = format!("Bearer {TOKEN}");

        // A Slack-host redirect, absolute and relative, keeps the authorization on every hop.
        for (start, last_host) in [
            ("/start-slack", "api.slack.com"),
            ("/start-relative", "files.slack.com"),
        ] {
            let response = http
                .send(get(&fake, "files.slack.com", start).bearer(bearer()))
                .await
                .expect(start);
            assert_eq!((response.status(), response.body()), (200, &b"final"[..]));
            let seen = fake.seen();
            let hops: Vec<_> = seen.iter().rev().take(2).collect();
            assert_eq!(hops[0].target, "/final");
            assert!(hops[0].host.starts_with(last_host), "{}", hops[0].host);
            assert_eq!(hops[0].authorization.as_deref(), Some(want.as_str()));
            assert_eq!(hops[1].target, start);
            assert_eq!(hops[1].authorization.as_deref(), Some(want.as_str()));
        }

        // Every other destination is refused before a request: the fake never sees "/x".
        for (start, error) in [
            ("/to-lookalike", HttpError::Destination),
            ("/to-sub-lookalike", HttpError::Destination),
            ("/to-foreign", HttpError::Destination),
            ("/to-ip", HttpError::Destination),
            ("/to-downgrade", HttpError::Destination),
            ("/to-userinfo", HttpError::Destination),
            ("/to-userinfo-trick", HttpError::Destination),
            ("/to-port", HttpError::Destination),
            ("/to-scheme-relative", HttpError::Destination),
            ("/to-malformed", HttpError::Redirect),
        ] {
            assert_eq!(
                http.send(get(&fake, "files.slack.com", start).bearer(bearer()))
                    .await
                    .unwrap_err(),
                error,
                "{start}"
            );
        }
        assert_eq!(count(&fake, "/x"), 0);

        // The same destinations as a first URL.
        for host in ["evilslack.com", "slack.com.evil.example", "other.example"] {
            let request = get(&fake, host, "/x").bearer(bearer());
            assert_eq!(
                http.send(request).await.unwrap_err(),
                HttpError::Destination,
                "{host}"
            );
        }
        let plain = Request::get(
            fake.url("files.slack.com", "/x")
                .replacen("https", "http", 1),
            LONG,
        )
        .bearer(bearer());
        assert_eq!(http.send(plain).await.unwrap_err(), HttpError::Destination);
        assert_eq!(count(&fake, "/x"), 0);

        // A redirect loop stops at its bound, and a request given no bearer sends none.
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/loop"))
                .await
                .unwrap_err(),
            HttpError::Redirect
        );
        assert_eq!(count(&fake, "/loop"), MAX_REDIRECTS + 1);
        http.send(get(&fake, "files.slack.com", "/bare"))
            .await
            .expect("bare");
        assert!(fake
            .seen()
            .iter()
            .filter(|s| s.target == "/bare" || s.target == "/loop")
            .all(|s| s.authorization.is_none()));

        // Nothing but Slack's own hosts was ever contacted.
        assert!(fake.seen().iter().all(
            |s| s.host.starts_with("files.slack.com:") || s.host.starts_with("api.slack.com:")
        ));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_mutation_is_sent_once_and_never_followed_to_a_redirect() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, port| match seen.target.as_str() {
            "/post" => Reply::redirect(&format!("https://api.slack.com:{port}/final")),
            _ => Reply::ok(b"reached"),
        })
        .await;
        let request = Request::post(
            fake.url("api.slack.com", "/post"),
            LONG,
            "application/json",
            b"{}".to_vec(),
        )
        .bearer(Secret::new(TOKEN));
        assert_eq!(
            fake.http().send(request).await.unwrap_err(),
            HttpError::Redirect
        );
        assert_eq!((count(&fake, "/post"), count(&fake, "/final")), (1, 0));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn an_untrusted_certificate_fails_before_any_http() {
    bounded(async {
        let rogue = FakeSlack::start(Identity::Rogue, |_, _| Reply::ok(b"served")).await;
        assert_eq!(
            rogue
                .http()
                .send(get(&rogue, "slack.com", "/"))
                .await
                .unwrap_err(),
            HttpError::Tls
        );
        eventually("the handshake failure", || rogue.tls_failures() == 1).await;
        assert!(rogue.seen().is_empty());
        rogue.shutdown().await;
        // The control: the same name and request succeed against the trusted identity.
        let trusted = FakeSlack::start(Identity::Trusted, |_, _| Reply::ok(b"served")).await;
        assert_eq!(
            trusted
                .http()
                .send(get(&trusted, "slack.com", "/"))
                .await
                .expect("trusted")
                .body(),
            b"served"
        );
        trusted.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_name_the_certificate_does_not_cover_fails_before_any_http() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |_, _| Reply::ok(b"served")).await;
        let http = fake.http();
        assert_eq!(
            http.send(get(&fake, "nosan.slack.com", "/"))
                .await
                .unwrap_err(),
            HttpError::Tls
        );
        eventually("the handshake failure", || fake.tls_failures() == 1).await;
        assert!(fake.seen().is_empty());
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/"))
                .await
                .expect("a covered name")
                .status(),
            200
        );
        // A name the test resolver does not know is refused before any connection.
        let before = fake.connections();
        assert_eq!(
            http.send(get(&fake, "unlisted.slack.com", "/"))
                .await
                .unwrap_err(),
            HttpError::Connect
        );
        assert_eq!(fake.connections(), before);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn chat_operations_keep_bounds_after_caller_drop() {
    bounded(async {
        let head = |length: &str| {
            format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n").into_bytes()
        };
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            match seen.target.as_str() {
                "/hang-headers" | "/held" => Reply::Hang,
                "/hang-body" => Reply::HangAfter([head("100"), b"abc".to_vec()].concat()),
                // Claims a gigabyte and sends two MiB; claims five bytes and sends two MiB.
                "/lie-long" => Reply::Raw([head("1073741824"), vec![b'x'; 2 << 20]].concat()),
                "/lie-short" => Reply::Raw([head("5"), vec![b'x'; 2 << 20]].concat()),
                "/flood" => Reply::Flood(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec()),
                "/exact" => Reply::ok(&vec![b'y'; MAX_BODY]),
                "/over" => Reply::ok(&vec![b'y'; MAX_BODY + 1]),
                _ => Reply::ok(b"ok"),
            }
        })
        .await;
        let http = fake.http();
        let short = |path: &str| Request::get(fake.url("files.slack.com", path), SHORT);

        // Stalled headers and a stalled body end at the deadline.
        for path in ["/hang-headers", "/hang-body"] {
            let start = Instant::now();
            assert_eq!(
                http.send(short(path)).await.unwrap_err(),
                HttpError::Timeout,
                "{path}"
            );
            assert!(
                start.elapsed() >= Duration::from_millis(300)
                    && start.elapsed() < Duration::from_secs(5),
                "{path}"
            );
        }

        // Bodies stay bounded whatever Content-Length claims.
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/lie-long"))
                .await
                .unwrap_err(),
            HttpError::TooLarge
        );
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/lie-short"))
                .await
                .expect("short claim")
                .body(),
            b"xxxxx"
        );
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/flood"))
                .await
                .unwrap_err(),
            HttpError::TooLarge
        );
        eventually("the endless writer to end", || fake.flood().1).await;
        assert!(
            fake.flood().0 < 64 << 20,
            "{} bytes written",
            fake.flood().0
        );
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/exact"))
                .await
                .expect("exact")
                .body()
                .len(),
            MAX_BODY
        );
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/over"))
                .await
                .unwrap_err(),
            HttpError::TooLarge
        );

        // Sixteen operations are admitted; callers that disappear do not free their permits; the next is busy.
        let deadline = Duration::from_secs(3);
        let started = Instant::now();
        let held: Vec<_> = (0..MAX_OPERATIONS)
            .map(|_| {
                let (http, request) = (
                    http.clone(),
                    Request::get(fake.url("files.slack.com", "/held"), deadline),
                );
                tokio::spawn(async move { http.send(request).await })
            })
            .collect();
        eventually("sixteen requests in flight", || {
            count(&fake, "/held") == MAX_OPERATIONS
        })
        .await;
        for caller in &held {
            caller.abort();
        }
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/after"))
                .await
                .unwrap_err(),
            HttpError::Busy
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            http.send(get(&fake, "files.slack.com", "/after"))
                .await
                .unwrap_err(),
            HttpError::Busy
        );
        assert_eq!(count(&fake, "/after"), 0);
        // The permits return when the owners' deadlines pass, not before.
        loop {
            match http.send(get(&fake, "files.slack.com", "/after")).await {
                Err(HttpError::Busy) => tokio::time::sleep(Duration::from_millis(50)).await,
                other => {
                    assert_eq!(other.expect("after the deadlines").body(), b"ok");
                    break;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "the permits never returned"
            );
        }
        assert!(
            started.elapsed() >= deadline - Duration::from_millis(200),
            "released early: {:?}",
            started.elapsed()
        );
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn chat_secrets_stay_inside_daemon_boundaries() {
    bounded(async {
        let private = |fake: &FakeSlack, host: &str| fake.url(host, &format!("/files-pri/{PRIVATE}?t={TOKEN}"));
        let fake = FakeSlack::start(Identity::Trusted, |seen, port| match seen.target.split('?').next().unwrap() {
            // A malformed answer that echoes the credential and the private URL.
            p if p.ends_with("malformed") => Reply::Raw(format!("HTTP/1.1 200 OK\r\nContent-Length: 999\r\nX-Echo: {TOKEN} {PRIVATE}\r\n\r\n{TOKEN}").into_bytes()),
            p if p.ends_with("redirect") => Reply::redirect(&format!("https://evilslack.com:{port}/{PRIVATE}?t={TOKEN}")),
            p if p.ends_with("hang") => Reply::Hang,
            _ => Reply::ok(format!("{TOKEN} {PRIVATE}").as_bytes()),
        })
        .await;
        let http = fake.http();
        let bearer = || Secret::new(TOKEN);
        let mut sink = String::new();
        let mut note = |text: String| sink.push_str(&text);

        let tail = |t: &str| format!("/files-pri/{PRIVATE}/{t}?t={TOKEN}");
        let cases = [
            ("malformed", HttpError::Protocol),
            ("redirect", HttpError::Destination),
            ("hang", HttpError::Timeout),
        ];
        for (kind, error) in cases {
            let request = Request::get(fake.url("files.slack.com", &tail(kind)), SHORT).bearer(bearer());
            note(format!("{request:?}"));
            let got = http.send(request).await;
            assert_eq!(got.as_ref().unwrap_err(), &error, "{kind}");
            note(format!("{got:?}"));
            note(got.unwrap_err().to_string());
        }
        // A transport error: an unknown name; a TLS error: the rogue identity; a credential that is no header value.
        let request = Request::get(private(&fake, "unlisted.slack.com"), SHORT).bearer(bearer());
        let got = http.send(request).await.unwrap_err();
        assert_eq!(got, HttpError::Connect);
        note(format!("{got} {got:?}"));
        let rogue = FakeSlack::start(Identity::Rogue, |_, _| Reply::ok(b"x")).await;
        let got = rogue.http().send(Request::get(private(&rogue, "slack.com"), LONG).bearer(bearer())).await.unwrap_err();
        assert_eq!(got, HttpError::Tls);
        note(format!("{got} {got:?}"));
        rogue.shutdown().await;
        let got = http.send(get(&fake, "files.slack.com", "/x").bearer(Secret::new(format!("{TOKEN}\n")))).await.unwrap_err();
        assert_eq!(got, HttpError::Invalid);
        note(format!("{got} {got:?}"));
        // A good answer that echoes both, and the credential itself.
        let ok = http.send(Request::get(private(&fake, "files.slack.com"), LONG).bearer(bearer())).await.expect("echo");
        assert!(String::from_utf8_lossy(ok.body()).contains(TOKEN));
        note(format!("{ok:?}"));
        note(format!("{:?} {}", bearer(), bearer()));

        assert!(!sink.is_empty());
        for secret in [TOKEN, PRIVATE, "files-pri"] {
            assert!(!sink.contains(secret), "{secret} reached a formatted value: {sink}");
        }
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn one_send_is_one_request_even_when_the_server_closes_mid_response() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| match seen.target.as_str() {
            "/close" => Reply::Drop,
            "/cut" => Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial".to_vec()),
            _ => Reply::ok(b"ok"),
        })
        .await;
        let http: Http = fake.http();
        // A first answer leaves a kept-alive server side; the library must not reuse that connection for a mutation.
        http.send(get(&fake, "api.slack.com", "/first"))
            .await
            .expect("first");
        let post = Request::post(
            fake.url("api.slack.com", "/close"),
            LONG,
            "application/json",
            b"{}".to_vec(),
        )
        .bearer(Secret::new(TOKEN));
        assert_eq!(http.send(post).await.unwrap_err(), HttpError::Protocol);
        assert_eq!(
            http.send(get(&fake, "api.slack.com", "/cut"))
                .await
                .unwrap_err(),
            HttpError::Protocol
        );
        // A retry would be immediate; give it room, then count.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            (
                count(&fake, "/first"),
                count(&fake, "/close"),
                count(&fake, "/cut")
            ),
            (1, 1, 1)
        );
        let posted = fake
            .seen()
            .into_iter()
            .find(|s| s.target == "/close")
            .unwrap();
        assert_eq!(
            (posted.method.as_str(), posted.body.as_slice()),
            ("POST", &b"{}"[..])
        );
        assert_eq!(fake.connections(), 3, "one fresh connection per request");
        fake.shutdown().await;
    })
    .await;
}
