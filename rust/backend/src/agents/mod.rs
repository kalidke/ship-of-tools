//! Account discovery, the folder-trust record, the awareness env, the launch recipe and the spawn env (backend agents).

pub(crate) mod accounts;
pub(crate) mod argv;
pub(crate) mod awareness;
pub(crate) mod env;
pub(crate) mod folder_trust;
pub(crate) mod memory;
pub(super) mod ops;
pub(crate) mod trust_declaration;

#[cfg(test)]
mod support_tests;
