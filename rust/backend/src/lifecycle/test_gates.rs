//! Held points for the daemon-lifetime harness (feature `daemon-lifetime-faults`; an installed binary is built without
//! it). `SOT_TEST_GATES` names a folder. A case engages a point by creating `<point>.hold` there and opens it by creating
//! `<point>`: until then the daemon waits at the point, for at most a minute. A point nobody engaged does not wait, so an
//! ordering the product leaves to chance (a close begun before an update's decision, an update committed before a close)
//! is the case's to choose, not the clock's. A point selects when something happens and does nothing else.

use std::path::PathBuf;
use std::time::{Duration, Instant};

const HELD_AT_MOST: Duration = Duration::from_secs(60);

/// Whether this daemon was started with held points (`SOT_TEST_GATES` set).
pub(crate) fn enabled() -> bool {
    std::env::var_os("SOT_TEST_GATES").is_some()
}

fn file(name: &str) -> Option<PathBuf> {
    std::env::var_os("SOT_TEST_GATES").map(|folder| PathBuf::from(folder).join(name))
}

/// Wait at the point `name`, if a case engaged it; a point never holds a runtime thread.
pub(crate) async fn held(name: &str) {
    let (Some(open), Some(engaged)) = (file(name), file(&format!("{name}.hold"))) else {
        return;
    };
    let deadline = Instant::now() + HELD_AT_MOST;
    while engaged.exists() && !open.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until the case creates the file `name`, however long that takes: the first automatic check starts when it does.
pub(crate) async fn wait(name: &str) {
    let Some(open) = file(name) else {
        return;
    };
    while !open.exists() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
