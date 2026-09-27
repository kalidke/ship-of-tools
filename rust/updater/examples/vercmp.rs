//! vercmp — does a candidate release tag sort strictly above every tag already
//! on its line?
//!
//!   cargo run -q -p sot-updater --example vercmp -- <candidate> [<existing>...]
//!
//! Exit 0: `<candidate>` sorts strictly above every `<existing>` (vacuously so
//! when none are given). Exit 1: it does not, and the highest `<existing>` is
//! printed on stdout. Exit 2: usage.
//!
//! It exists so `scripts/release.sh` can enforce release ordering with the
//! updater's own `compare_versions` — the same comparison every installed box
//! uses to decide what "newer" means — instead of a second semver
//! implementation in shell, which is the failure this guard is here to prevent.
//! An example rather than a binary: nothing ships it, but `cargo test` builds
//! it, so it cannot rot away from the module it calls.

use std::cmp::Ordering;

use sot_updater::semver::compare_versions;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(candidate) = args.next() else {
        eprintln!("usage: vercmp <candidate> [<existing>...]");
        std::process::exit(2);
    };
    // max_by, not sort: the same selection select.rs makes over release tags.
    if let Some(highest) = args.max_by(|a, b| compare_versions(a, b)) {
        if compare_versions(&candidate, &highest) != Ordering::Greater {
            println!("{highest}");
            std::process::exit(1);
        }
    }
}
