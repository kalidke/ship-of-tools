//! Reset: the pointer rename-aside and the recovery of a crashed reset, as journaled.

use crate::supervisor::*;

// ---------------------------------------------------------------------
// Reset
// ---------------------------------------------------------------------

/// Rename the current pointer aside (evidence-preserving, no-replace) if
/// one exists, then mint `new_voyage` fresh. `aside_name` is the exact
/// filename to rename it to: `Some(name)` for the live, journaled path
/// (chosen and recorded AT ADMISSION time); `None` for the no-supervisor
/// CLI path, which journals nothing and so mints its own name here.
pub(in crate::supervisor) fn reset_pointer(state_dir: &Path, new_voyage: &str, aside_name: Option<&str>) -> crate::Result<()> {
    let live = pointer::pointer_path(state_dir);
    if live.exists() {
        crate::host::fsync_file(&live).map_err(|e| {
            state_unless_full(e, |e| format!("reset_pointer: flushing the live pointer {live:?} before renaming it aside: {e}"))
        })?;
        let owned;
        let name: &str = match aside_name {
            Some(n) => n,
            None => {
                owned = mint_aside_name()?;
                &owned
            }
        };
        let aside = state_dir.join(name);
        crate::host::publish_noreplace(&live, &aside).map_err(|e| {
            state_unless_full(e, |e| format!("reset_pointer: renaming {live:?} aside to {aside:?}: {e}"))
        })?;
    }
    std::fs::create_dir_all(voyages_dir(state_dir)).map_err(|e| {
        state_unless_full(e.into(), |e| format!("reset_pointer: creating {:?}: {e}", voyages_dir(state_dir)))
    })?;
    let root = voyage_root_path(state_dir, new_voyage);
    if !root.exists() {
        VoyageStore::bootstrap(&root, new_voyage, RetentionClass::Archive)
            .map_err(|e| state_unless_full(e, |e| format!("reset_pointer: bootstrapping {root:?}: {e}")))?;
    }
    pointer::publish(state_dir, new_voyage).map_err(|e| {
        state_unless_full(e, |e| format!("reset_pointer: publishing the new pointer for {new_voyage:?}: {e}"))
    })?;
    Ok(())
}

/// A failed step of a reset is a `State` error with its words, except
/// storage exhaustion, which is returned as itself so the authority waits for
/// storage instead of failing the reset (`do_reset`).
fn state_unless_full(e: crate::Error, text: impl FnOnce(&crate::Error) -> String) -> crate::Error {
    if storage_exhaustion(&e).is_some() {
        e
    } else {
        err_state(text(&e))
    }
}

pub(in crate::supervisor) fn reconcile_reset(
    state_dir: &Path,
    op_id: &str,
    new_voyage: &str,
    old_voyage: Option<&str>,
    aside: Option<&str>,
) -> crate::Result<()> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) if id == new_voyage => {}
        PointerState::Valid(id) if Some(id.as_str()) == old_voyage => {
            reset_pointer(state_dir, new_voyage, aside)?;
        }
        PointerState::NotFound => match (old_voyage, aside) {
            (Some(_), Some(aside_name)) => {
                if !state_dir.join(aside_name).exists() {
                    return Err(err_state(format!(
                        "reset {op_id}: the pointer is absent but its recorded evidence rename \
                         {aside_name:?} does not exist — an operator must investigate before this \
                         can be resumed"
                    )));
                }
                reset_pointer(state_dir, new_voyage, aside)?;
            }
            (Some(_), None) => {
                return Err(err_state(format!(
                    "reset {op_id}: a pointer existed at admission but no aside filename was \
                     journaled for it — cannot verify the pointer's disappearance is this \
                     operation's own doing"
                )));
            }
            (None, _) => {
                reset_pointer(state_dir, new_voyage, None)?;
            }
        },
        PointerState::Valid(_) => {
            return Err(err_state(format!(
                "reset {op_id}: the pointer names a THIRD identity — an operator must investigate; \
                 minting yet another would be exactly the double-mint at-most-once forbids"
            )));
        }
        PointerState::Corrupt | PointerState::OtherIo(_) => {
            return Err(err_state(format!("reset {op_id}: the pointer is unreadable during recovery")));
        }
    }
    journal::finish(
        state_dir,
        op_id,
        &journal::TerminalRecord::ResetDone { new_voyage: new_voyage.to_string() },
    )
}

#[cfg(test)]
#[path = "reset_tests.rs"]
mod tests;
