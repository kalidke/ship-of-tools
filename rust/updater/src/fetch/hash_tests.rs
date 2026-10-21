use super::*;

#[test]
fn sha256_known_answers() {
    // FIPS-180-4 / NIST test vectors (carried over from the hand-rolled
    // implementation this replaced).
    assert_eq!(
        sha256_bytes(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_bytes(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_bytes(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[tokio::test]
async fn file_and_bytes_agree() {
    let dir = std::env::temp_dir().join(format!("sot-updater-hash-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let p = dir.join("blob");
    // Cross a chunk boundary to exercise streaming.
    let data = vec![0xABu8; 3 * 1024 * 1024 + 17];
    tokio::fs::write(&p, &data).await.unwrap();
    assert_eq!(sha256_file(&p).await.unwrap(), sha256_bytes(&data));
    tokio::fs::remove_dir_all(&dir).await.unwrap();
}
