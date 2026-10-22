use super::*;

#[tokio::test]
async fn quarto_resolver_error_precedes_spawn() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = crate::paths::EnvGuard::capture("SOT_JULIA_BIN");
    std::env::set_var("SOT_JULIA_BIN", "relative-julia");
    let refusal = crate::sidecars::julia::resolve_bin().unwrap_err().to_string();
    let dir = tempfile::tempdir().unwrap();
    // An owned program that does not exist: starting it would be a not-found error, not the resolver's.
    let missing = dir.path().join("no-such-quarto").to_string_lossy().into_owned();
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    for execute in [true, false] {
        let err = run_quarto(&missing, dir.path(), std::ffi::OsStr::new("doc.qmd"), "out.html", execute, QUARTO_RUN_BOUND, sig)
            .await
            .expect_err("a refused julia must fail the render");
        assert_eq!(err.kind(), std::io::ErrorKind::Other, "execute={execute}");
        assert_eq!(err.to_string(), refusal, "execute={execute}");
    }
    assert_eq!(sig.live(), 0);
}
