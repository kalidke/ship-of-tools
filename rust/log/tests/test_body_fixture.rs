//! Real libtest bodies for scripts/tests/test-test-body.sh. Explicit fixture input records body
//! execution under the caller's scratch root; ordinary workspace execution is harmless. This fixture
//! starts no process and opens no endpoint.

use std::path::PathBuf;

fn witness(name: &str) -> Option<PathBuf> {
    let root = std::env::current_dir().unwrap();
    if !root.join("iso-sh-fixture-request").is_file() {
        return None;
    }
    std::fs::write(root.join(format!("witness-{name}")), name).unwrap();
    Some(root)
}

#[test]
fn ordinary() {
    if let Some(root) = witness("ordinary") {
        assert!(
            !root.join("iso-sh-panic-request").exists(),
            "requested fixture panic"
        );
    }
}

#[test]
fn positive() {
    witness("positive");
}

#[test]
fn misleading() {
    witness("misleading");
    println!("running 1 test\ntest absent ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s");
}

#[test]
fn near() {
    witness("near");
}

mod qualified {
    #[test]
    fn near() {
        super::witness("qualified-near");
    }

    #[test]
    #[ignore]
    fn ignored_positive() {
        if let Some(root) = super::witness("ignored-positive") {
            std::fs::write(root.join("rust.ready"), "ready").unwrap();
            assert!(root.join("go").exists(), "go must precede completion");
        }
        println!("filed fixture\nroute local");
    }
}

#[test]
#[ignore]
fn ignored_panic() {
    witness("ignored-panic");
    panic!("requested ignored fixture panic");
}

#[test]
#[ignore]
fn t11_rust_appends_200() {
    if let Some(root) = witness("appender") {
        std::fs::write(root.join("rust.ready"), "ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !root.join("go").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "go never appeared in {}",
                root.display()
            );
            std::thread::yield_now();
        }
        assert!(
            !root.join("iso-sh-panic-request").exists(),
            "requested appender panic"
        );
    }
    println!("filed fixture\nroute local");
}

fn ping(name: &str) {
    if let Some(root) = witness(name) {
        assert!(
            !root.join("iso-sh-panic-request").exists(),
            "requested wake fixture panic"
        );
        let path = PathBuf::from(std::env::var_os("SOT_E2E_PING_LOG").unwrap());
        let parent = path.parent().unwrap().canonicalize().unwrap();
        assert!(
            parent.starts_with(root.canonicalize().unwrap()),
            "ping outside scratch root"
        );
        std::fs::write(path, "100 ping\n").unwrap();
    }
}

#[test]
#[ignore]
fn comm_wake_e2e() {
    ping("wake");
}

mod nested {
    #[test]
    #[ignore]
    fn comm_wake_e2e() {
        super::ping("nested-wake");
    }
}
