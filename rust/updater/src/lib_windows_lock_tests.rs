use super::*;
use std::os::windows::fs::OpenOptionsExt;

/// No sharing at all — the shape an antivirus scanner's open handle
/// takes. Held for 10s, then released; the commit must still succeed,
/// via the real (non-fast) [`RENAME_BACKOFF`].
#[tokio::test]
async fn commit_survives_an_antivirus_style_open_handle() {
    let root = std::env::temp_dir().join(format!(
        "sot-updater-winlock-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = tokio::fs::remove_dir_all(&root).await;
    tokio::fs::create_dir_all(&root).await.unwrap();
    let tmp = root.join("tmp-v9.9.9-windows-x86_64");
    tokio::fs::create_dir_all(&tmp).await.unwrap();
    let locked_file = tmp.join("sotd.exe");
    tokio::fs::write(&locked_file, b"be-binary").await.unwrap();
    let dest = root.join("v9.9.9-windows-x86_64");

    const FILE_SHARE_NONE: u32 = 0;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_NONE)
        .open(&locked_file)
        .expect("must be able to open the file exclusively to simulate a scanner's lock");

    let releaser = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        drop(handle);
    });

    let result = commit_stage(&dest, &root, RENAME_BACKOFF, || async {
        tokio::fs::rename(&tmp, &dest).await
    })
    .await;

    releaser.await.unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert!(dest.is_dir());

    tokio::fs::remove_dir_all(&root).await.unwrap();
}
