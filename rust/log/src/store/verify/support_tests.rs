//! Test helper shared by the verifier tests: a bootstrapped voyage store.

use super::*;
use crate::store::segment::RetentionClass;
use crate::store::voyage::VoyageStore;

pub(super) fn store(dir: &Path, name: &str) -> VoyageStore {
    let root = dir.join(name);
    VoyageStore::bootstrap(&root, name, RetentionClass::Discard).unwrap();
    VoyageStore::open_for_writing(&root, name).unwrap()
}
