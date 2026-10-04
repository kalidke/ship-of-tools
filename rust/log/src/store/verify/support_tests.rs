//! Test helper shared by the verifier tests: a bootstrapped voyage store.

use super::*;
use crate::segment::RetentionClass;
use crate::voyage::VoyageStore;

pub(super) fn store(dir: &Path, name: &str) -> VoyageStore {
    let root = dir.join(name);
    VoyageStore::bootstrap(&root, name, RetentionClass::Discard).unwrap();
    VoyageStore::open_for_writing(&root, name).unwrap()
}
