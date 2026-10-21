// build.rs — builds the one native launcher (host::process_tree): `native_birth.c`, on the Unix targets, into the
// library the `unix_birth` wrapper links. A test-only variant with phase barriers is built under the `native-barrier`
// feature and never in an installed build.

/// Compile the native launcher for a Unix target. A macOS target built from another host has no SDK to include,
/// and a `cargo check` there never links, so the compile is skipped with a warning; every macOS build that ships is
/// native (release.yml builds it on a macOS runner).
fn build_native_birth() {
    let dir = "src/host/process_tree";
    println!("cargo:rerun-if-changed={dir}/native_birth.c");
    println!("cargo:rerun-if-changed={dir}/native_birth.h");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_NATIVE_BARRIER");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    if !family.split(',').any(|f| f == "unix") {
        return;
    }
    let host = std::env::var("HOST").unwrap_or_default();
    if target_os == "macos" && !host.contains("apple-darwin") {
        println!("cargo:warning=native_birth.c is not compiled for a macOS target built from another host");
        return;
    }
    let mut build = cc::Build::new();
    build
        .file(format!("{dir}/native_birth.c"))
        .include(dir)
        .warnings(true)
        .extra_warnings(true);
    if std::env::var_os("CARGO_FEATURE_NATIVE_BARRIER").is_some() {
        build.define("SOT_BIRTH_FAULTS", None);
    }
    build.compile("sot_native_birth");
}

fn main() {
    build_native_birth();
}
