use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

/// Sub-millisecond entries — this schedule is under test for retry
/// COUNT and eventual outcome, not real wall-clock backoff.
const FAST_BACKOFF: &[Duration] = &[
    Duration::from_millis(1),
    Duration::from_millis(1),
    Duration::from_millis(1),
];

fn tmp_dir_names(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("tmp-"))
                .collect()
        })
        .unwrap_or_default()
}

fn scratch_root(case: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sot-updater-retry-{case}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// The commit fails the first two attempts (simulating the file still
/// being held), succeeds on the third — and only THEN does the tree
/// change: through the failing attempts, `tmp` is untouched and no
/// second `tmp-*` appears (item 2 — one tmp, reused, never re-created).
/// Two abandoned `tmp-*` dirs from earlier, never-committed releases sit
/// alongside it; the successful commit sweeps them (item 3), unlike the
/// age-gated `sweep_stale_tmp` which would leave them for 24h.
#[tokio::test]
async fn commit_retries_then_succeeds_and_sweeps_stray_tmp_dirs() {
    let root = scratch_root("success");
    let _ = tokio::fs::remove_dir_all(&root).await;
    tokio::fs::create_dir_all(&root).await.unwrap();

    let tmp = root.join("tmp-v9.9.9-linux-x86_64");
    tokio::fs::create_dir_all(&tmp).await.unwrap();
    let stray_a = root.join("tmp-v9.9.7-linux-x86_64");
    let stray_b = root.join("tmp-v9.9.8-linux-x86_64");
    tokio::fs::create_dir_all(&stray_a).await.unwrap();
    tokio::fs::create_dir_all(&stray_b).await.unwrap();
    let dest = root.join("v9.9.9-linux-x86_64");

    let attempts = AtomicU32::new(0);
    let result = commit_stage(&dest, &root, FAST_BACKOFF, || {
        let n = attempts.fetch_add(1, Ordering::SeqCst);
        let tmp = tmp.clone();
        let dest = dest.clone();
        let root = root.clone();
        async move {
            if n < 2 {
                // The rename never ran — the tree, including the exactly
                // one live `tmp-*`, is exactly as it was.
                assert_eq!(tmp_dir_names(&root).len(), 3);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Access is denied. (os error 5)",
                ));
            }
            tokio::fs::rename(&tmp, &dest).await
        }
    })
    .await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert!(dest.is_dir());
    assert!(!tmp.exists());
    assert!(!stray_a.exists());
    assert!(!stray_b.exists());
    assert!(tmp_dir_names(&root).is_empty());

    tokio::fs::remove_dir_all(&root).await.unwrap();
}

/// A commit that never succeeds exhausts the backoff and surfaces the
/// last (real) error — the shape `rust/backend/src/update.rs` turns into
/// `update blocked: <error>`.
#[tokio::test]
async fn commit_exhausts_backoff_and_surfaces_the_last_error() {
    let root = scratch_root("exhausted");
    let _ = tokio::fs::remove_dir_all(&root).await;
    tokio::fs::create_dir_all(&root).await.unwrap();
    let dest = root.join("v9.9.9-linux-x86_64");

    let attempts = AtomicU32::new(0);
    let result = commit_stage(&dest, &root, FAST_BACKOFF, || {
        attempts.fetch_add(1, Ordering::SeqCst);
        async {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Access is denied. (os error 5)",
            ))
        }
    })
    .await;

    let err = result.expect_err("every attempt failed — must not report success");
    assert!(err.to_string().contains("Access is denied"), "{err}");
    // One initial attempt plus one retry per backoff entry.
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        FAST_BACKOFF.len() as u32 + 1
    );
    assert!(!dest.exists());

    tokio::fs::remove_dir_all(&root).await.unwrap();
}
