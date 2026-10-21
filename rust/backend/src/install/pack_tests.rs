use super::*;

fn sums(names: &[&str]) -> Vec<sot_updater::fetch::sums::SumEntry> {
    names.iter().map(|n| sot_updater::fetch::sums::SumEntry { sha256: "0".repeat(64), name: (*n).to_string() }).collect()
}

/// Verified before it runs: a pack names exactly one archive for this system, its version spelled safely; a binary is
/// the pack's when it says the pack's version at the pack's commit; a pack whose archive or COMMIT does not match its
/// SHA256SUMS, or whose binaries say another build, installs nothing. That a backend-only run never starts the
/// frontend is the entry suite's (installer-entry.sh, a pack whose `sot` cannot run).
#[test]
fn a_pack_is_verified_before_it_runs() {
    assert_eq!(archive_version(&sums(&["sot-0.6.7-linux-x86_64.tar.gz", "COMMIT", "sot-0.6.7-windows-x86_64.zip"]), "linux-x86_64").as_deref(), Ok("0.6.7"));
    assert_eq!(archive_version(&sums(&["sot-0.6.7-dev+abc-linux-x86_64.tar.gz"]), "linux-x86_64").as_deref(), Ok("0.6.7-dev+abc"));
    assert!(archive_version(&sums(&["COMMIT"]), "linux-x86_64").is_err());
    assert!(archive_version(&sums(&["sot-1-linux-x86_64.tar.gz", "sot-2-linux-x86_64.tar.gz"]), "linux-x86_64").is_err());
    assert!(archive_version(&sums(&["sot-1$x-linux-x86_64.tar.gz"]), "linux-x86_64").unwrap_err().contains("unsafe version"));

    let c = "0123456789abcdef0123456789abcdef01234567";
    assert!(binary_says("sotd 0.6.7 (012345678 2026-10-20, protocol 9)", "sotd", "0.6.7", c));
    assert!(binary_says("sot 0.6.7 (protocol 9)", "sot", "0.6.7", c));
    assert!(!binary_says("sotd 0.6.7 (999999999 2026-10-20, protocol 9)", "sotd", "0.6.7", c));
    assert!(!binary_says("sotd 0.6.6 (012345678 2026-10-20)", "sotd", "0.6.7", c));
    assert!(!binary_says("sot 0.6.7 (012345678 x)", "sotd", "0.6.7", c));

    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let c = "0123456789abcdef0123456789abcdef01234567";
        let make = |says: &str| {
            let bin = d.join("sot-0.6.7-linux-x86_64");
            std::fs::create_dir_all(&bin).unwrap();
            for name in ["sotd", "sot"] {
                sot_log::test_exec::write_executable(&bin.join(name), format!("#!/bin/sh\necho '{name} {says} (012345678 2026-10-20, protocol 9)'\n"));
            }
            std::fs::write(d.join("sot-0.6.7-linux-x86_64.tar.gz"), b"archive").unwrap();
            std::fs::write(d.join("COMMIT"), format!("{c}\n")).unwrap();
            let sum = |p: &str| sot_updater::fetch::hash::sha256_bytes(&std::fs::read(d.join(p)).unwrap());
            std::fs::write(d.join("SHA256SUMS"), format!("{}  sot-0.6.7-linux-x86_64.tar.gz\n{}  COMMIT\n", sum("sot-0.6.7-linux-x86_64.tar.gz"), sum("COMMIT"))).unwrap();
        };
        make("0.6.7");
        let pack = read(d, "linux-x86_64", Some("v0.6.7")).unwrap();
        assert_eq!((pack.version.as_str(), pack.short(), pack.tag()), ("0.6.7", "012345678", "v0.6.7".to_string()));
        assert!(read(d, "linux-x86_64", Some("v0.6.8")).unwrap_err().contains("not v0.6.8"));
        make("0.6.6");
        assert!(read(d, "linux-x86_64", None).unwrap_err().contains("does not match 0.6.7"));
        make("0.6.7");
        std::fs::write(d.join("COMMIT"), "fedcba9876543210fedcba9876543210fedcba98\n").unwrap();
        assert!(read(d, "linux-x86_64", None).unwrap_err().contains("COMMIT does not match its SHA256SUMS entry"));
    }
}
