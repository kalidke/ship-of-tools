//! Minimal CLI for the sot-log crate (ADR 0039 §"Verifier and gates").
//!
//! Usage: sot-log verify <voyage_root> <voyage_id>
//!
//! Exit codes: 0 = verified, 1 = verification error, 2 = usage error.

use std::path::{Path, PathBuf};

fn usage() -> ! {
    eprintln!("usage: sot-log verify <voyage_root> <voyage_id>");
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else { usage() };
    if cmd != "verify" {
        eprintln!("unknown subcommand: {cmd}");
        usage();
    }
    let Some(root) = args.next() else { usage() };
    let Some(voyage_id) = args.next() else { usage() };
    if args.next().is_some() {
        eprintln!("too many arguments");
        usage();
    }

    let root = PathBuf::from(root);
    match sot_log::verify::verify_voyage(&root, &voyage_id) {
        Ok(()) => {
            println!(
                "ok: {} segments verified",
                count_segments(&root.join("seg"))
            );
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

/// Count files in `seg_dir` that name a segment (skips anything
/// unparseable — mirrors `verify_voyage`'s own enumeration).
fn count_segments(seg_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(seg_dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| sot_log::SegmentIdentity::parse_file_name(n).is_some())
                .unwrap_or(false)
        })
        .count()
}
