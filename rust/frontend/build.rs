// Windows-only: embed the Ship of Tools logo into sot.exe as its icon resource.
//
// The runtime `with_window_icon`/`with_taskbar_icon` (gpu.rs) sets the icon on
// the live *window*, which drives Alt-Tab and the title bar. But Windows draws a
// running app's *taskbar button* from the app identity, which — with an explicit
// AppUserModelID (set in main.rs) — resolves to the *executable's own icon*. An
// exe with no icon resource falls back to the generic default (and a stale
// per-exe icon cache can pin that default even after the window sets ICON_BIG).
// Embedding the icon into the exe makes Explorer, the taskbar, and the icon cache
// all resolve to the wheel from the executable itself — robust across machines
// and launch paths. This complements (does not replace) the runtime window icon.
//
// `cfg(windows)`-gated so Linux/macOS builds are untouched (winresource is only a
// build-dependency on Windows).
//
// rc.exe discovery: winresource's own SDK auto-detection is unreliable when the
// build runs from a plain PowerShell rather than a Developer/vcvars prompt — which
// is exactly how scripts/relaunch-sot.ps1 builds. In that case `rc.exe` is not on
// PATH, `compile()` returns Err, the embed is silently skipped (the exe shipped
// with ZERO resources → generic taskbar icon; diagnosed 2026-07-03). The `cc`
// crate finds link.exe via its own vcvars probe, so the SDK *is* installed — we
// just have to point winresource at the SDK's rc.exe explicitly. We locate the
// newest Windows 10/11 SDK `rc.exe` under `Windows Kits\10\bin\<ver>\{x64,x86}` and
// hand winresource its directory via `set_toolkit_path`.
fn main() {
    #[cfg(windows)]
    {
        // logo.ico lives at the repo root; resolve it from this crate's manifest
        // dir so the path holds regardless of the build script's CWD.
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let ico = std::path::Path::new(&manifest).join("../../logo.ico");
        println!("cargo:rerun-if-changed={}", ico.display());

        let mut res = winresource::WindowsResource::new();
        res.set_icon(&ico.to_string_lossy());

        // Point winresource at a concrete SDK rc.exe directory when we can find
        // one, so the embed succeeds regardless of whether rc.exe is on PATH.
        match find_sdk_rc_dir() {
            Some(dir) => {
                println!("cargo:warning=embedding exe icon via SDK rc.exe at {dir}");
                res.set_toolkit_path(&dir);
            }
            None => {
                // No SDK rc.exe located — winresource will try its own discovery,
                // which may still work from a vcvars shell. If it can't, compile()
                // below degrades gracefully to a warning (exe keeps default icon).
                println!(
                    "cargo:warning=no Windows SDK rc.exe found under 'Windows Kits\\10\\bin'; \
                     relying on winresource auto-detection (exe icon may be skipped)"
                );
            }
        }

        if let Err(e) = res.compile() {
            // Non-fatal: a failed embed just means the exe keeps the default icon
            // (the runtime window icon still covers Alt-Tab / title bar).
            println!("cargo:warning=failed to embed exe icon: {e}");
        }
    }
}

/// Find the directory containing the newest Windows SDK `rc.exe`.
///
/// Searches `<ProgramFiles*>\Windows Kits\10\bin\<version>\{x64,x86}\rc.exe` and
/// returns the directory of the highest-versioned match. `bin\<version>` dirs are
/// named like `10.0.26100.0`; we compare them numerically so 10.0.26100 beats
/// 10.0.9999. Returns the directory (not the exe path) — `set_toolkit_path` wants
/// the folder that holds rc.exe.
#[cfg(windows)]
fn find_sdk_rc_dir() -> Option<String> {
    use std::path::PathBuf;

    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"] {
        if let Ok(p) = std::env::var(var) {
            let root = PathBuf::from(p).join("Windows Kits").join("10").join("bin");
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }

    // (version_key, dir-holding-rc.exe) for the best match so far.
    let mut best: Option<(Vec<u64>, String)> = None;
    for root in roots {
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for ent in entries.flatten() {
            let ver = ent.file_name().to_string_lossy().into_owned();
            // Only consider SDK version dirs (e.g. "10.0.26100.0").
            if !ver.starts_with("10.") {
                continue;
            }
            let key: Vec<u64> = ver.split('.').map(|s| s.parse().unwrap_or(0)).collect();
            // Prefer x64 rc.exe, fall back to x86; both emit a link-compatible .res.
            for arch in ["x64", "x86"] {
                let dir = ent.path().join(arch);
                if dir.join("rc.exe").is_file() {
                    let better = match &best {
                        Some((bk, _)) => key > *bk,
                        None => true,
                    };
                    if better {
                        best = Some((key.clone(), dir.to_string_lossy().into_owned()));
                    }
                    break;
                }
            }
        }
    }

    best.map(|(_, dir)| dir)
}
