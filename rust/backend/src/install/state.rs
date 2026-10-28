//! The installer's state-folder decision, using the row launcher's qualification.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use sot_platform::host::env::Env;

use super::verdict::{Found, Mark, Stop};
use crate::rows::spawn::state_root::qualify_state_root;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Choice {
    Ok(PathBuf),
    Made { root: PathBuf, text: String },
    Todo(String),
}

impl Choice {
    pub(super) fn record(self, found: &mut Found) {
        match self {
            Self::Ok(root) => found.item(Mark::Ok, format!("state folder {}", root.display())),
            Self::Made { root, .. } => {
                found.item(Mark::Made, format!("state folder {}", root.display()))
            }
            Self::Todo(text) => found.step(text.replace(['\n', '\r'], " ")),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Parent {
    Absent,
    Owned,
    Foreign,
    Link,
    Other,
}

struct Candidate {
    base: PathBuf,
    scratch_dir: bool,
    parent: Parent,
    qualified: Result<PathBuf, String>,
}

impl Candidate {
    fn refusal(&self) -> Option<String> {
        let parent = self.base.parent().expect("candidate has an account folder");
        if !self.scratch_dir {
            return Some(format!(
                "{} is not a directory",
                parent.parent().expect("scratch folder").display()
            ));
        }
        match self.parent {
            Parent::Absent | Parent::Owned => None,
            Parent::Foreign => Some(format!("{} is owned by another uid", parent.display())),
            Parent::Link => Some(format!("{} is a symlink", parent.display())),
            Parent::Other => Some(format!(
                "{} is not a real, readable directory",
                parent.display()
            )),
        }
    }
}

fn dropin_text(base: &Path) -> String {
    format!("# Written by the Ship of Tools installer: this home is on a network filesystem, so the daemon keeps its\n# session records on this computer's own disk (ADR 0043 decision 23).\n[Service]\nEnvironment=XDG_STATE_HOME={}\n", base.display())
}

fn manual(root: &Path, why: &str, dropin: &Path) -> Choice {
    Choice::Todo(format!("state folder {}: {why}; make a folder on a local disk and write {} with it: mkdir -p /path/on/local/disk/state && chmod 700 /path/on/local/disk/state; mkdir -p {} && printf '[Service]\\nEnvironment=XDG_STATE_HOME=/path/on/local/disk/state\\n' > {}; systemctl --user daemon-reload && systemctl --user restart sotd.service", root.display(), dropin.display(), dropin.parent().expect("drop-in folder").display(), dropin.display()))
}

fn personal(root: &Path, why: &str, dropin: &Path) -> Choice {
    Choice::Todo(format!("state folder {}: {why}; {} is a person's file: leave it and point its XDG_STATE_HOME at a local disk, then run systemctl --user daemon-reload && systemctl --user restart sotd.service", root.display(), dropin.display()))
}

/// The decision reads only values. No environment, filesystem probe or service command is hidden in this table.
fn decide(
    xdg: Option<&OsStr>,
    root: &Path,
    verdict: Result<PathBuf, String>,
    candidate: Option<&Candidate>,
    dropin: &Path,
    existing: Option<&[u8]>,
) -> Choice {
    let why = match verdict {
        Ok(root) => return Choice::Ok(root),
        Err(why) => why,
    };
    if xdg.is_some_and(|v| !v.is_empty()) || candidate.is_none() {
        return Choice::Todo(format!("state folder {}: {why}; point XDG_STATE_HOME at a local disk where it is exported, then restart sotd", root.display()));
    }
    let candidate = candidate.expect("checked above");
    let text = dropin_text(&candidate.base);
    if existing.is_some_and(|bytes| bytes != text.as_bytes()) {
        return personal(root, &why, dropin);
    }
    if let Some(reason) = candidate.refusal() {
        return manual(root, &format!("{why}; {reason}"), dropin);
    }
    let qualified = match &candidate.qualified {
        Ok(root) => root,
        Err(reason) => return manual(root, &format!("{why}; {reason}"), dropin),
    };
    Choice::Made {
        root: qualified.clone(),
        text,
    }
}

/// lstat, never following an account-folder link under the shared scratch directory.
fn parent_at(path: &Path, uid: u32) -> Parent {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Parent::Absent,
        Err(_) => Parent::Other,
        Ok(m) if m.file_type().is_symlink() => Parent::Link,
        Ok(m) if !m.is_dir() => Parent::Other,
        Ok(m) if m.uid() != uid => Parent::Foreign,
        Ok(_) => Parent::Owned,
    }
}

fn candidate_at(scratch: &Path, user: &str, uid: u32) -> Candidate {
    use std::os::unix::fs::DirBuilderExt;
    let parent = scratch.join(user);
    let mut candidate = Candidate {
        base: parent.join("sot-state"),
        scratch_dir: scratch.is_dir(),
        parent: parent_at(&parent, uid),
        qualified: Err("not checked".into()),
    };
    if candidate.refusal().is_some() {
        return candidate;
    }
    // Atomic mkdir for a missing account folder, then recheck even if someone else won its creation.
    if matches!(candidate.parent, Parent::Absent) {
        if let Err(e) = std::fs::DirBuilder::new().mode(0o700).create(&parent) {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                candidate.qualified = Err(format!("could not create {}: {e}", parent.display()));
                return candidate;
            }
        }
        candidate.parent = parent_at(&parent, uid);
    }
    if candidate.refusal().is_none() {
        candidate.qualified = qualify_state_root(&candidate.base.join("sot"), None);
    }
    candidate
}

/// A link, directory, unreadable file or any other unrecognized entry belongs to the person, even if a link's target
/// happens to contain our text. None is an absent entry only.
fn existing_at(path: &Path) -> Option<Vec<u8>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Ok(m) if m.is_file() => Some(std::fs::read(path).unwrap_or_default()),
        _ => Some(Vec::new()),
    }
}

/// Publish only a missing drop-in, durably and without replacing a racing personal file. An identical file needs no
/// write; None means another writer placed personal bytes before publication.
fn publish(path: &Path, text: &str) -> Result<Option<bool>, Stop> {
    use std::io::Write;
    if let Some(bytes) = existing_at(path) {
        return Ok((bytes == text.as_bytes()).then_some(false));
    }
    let dir = path.parent().expect("drop-in folder");
    let write = || -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
        tmp.write_all(text.as_bytes())
            .and_then(|()| tmp.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        match sot_platform::host::publish_noreplace(tmp.path(), path) {
            Ok(()) => Ok(()),
            Err(sot_platform::Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    };
    write().map_err(|e| {
        Stop::at(
            "state folder",
            1,
            format!("writing {}: {e}", path.display()),
        )
    })?;
    Ok(existing_at(path)
        .filter(|b| b == text.as_bytes())
        .map(|_| true))
}

/// After the install gates and (for a managed service) its host pin. The scratch directory and account are inputs so
/// tests exercise real publication and privacy in their own temporary tree, without touching /scratch or a unit.
pub(super) fn setup(
    env: &Env,
    units: Option<&Path>,
    scratch: &Path,
    user: &str,
) -> Result<(Choice, bool), Stop> {
    let Some(root) = sot_platform::host::state_dir::sot_state_dir_in(env) else {
        return Ok((Choice::Todo("state folder: could not resolve this machine's state root ($XDG_STATE_HOME or $HOME unset); export XDG_STATE_HOME pointing at a local disk, then restart sotd".into()), false));
    };
    let verdict = qualify_state_root(&root, None);
    let dropin = units
        .unwrap_or(Path::new("~/.config/systemd/user"))
        .join("sotd.service.d/state.conf");
    let xdg = env.get_nonempty("XDG_STATE_HOME");
    if verdict.is_ok() || units.is_none() || xdg.is_some() {
        return Ok((decide(xdg, &root, verdict, None, &dropin, None), false));
    }
    // USER is the same account name enable-linger uses. Never let it introduce path components or unit syntax.
    if user.is_empty()
        || user == "."
        || user == ".."
        || user.contains('/')
        || super::unix::path_refused("account", user).is_some()
    {
        return Ok((
            manual(
                &root,
                &format!(
                    "{}; could not determine a safe account name for /scratch",
                    verdict.unwrap_err()
                ),
                &dropin,
            ),
            false,
        ));
    }
    let candidate = candidate_at(scratch, user, sot_platform::host::state_dir::current_uid());
    let existing = existing_at(&dropin);
    let choice = decide(
        xdg,
        &root,
        verdict.clone(),
        Some(&candidate),
        &dropin,
        existing.as_deref(),
    );
    if let Choice::Made { text, .. } = &choice {
        return match publish(&dropin, text)? {
            Some(changed) => Ok((choice, changed)),
            None => Ok((personal(&root, &verdict.unwrap_err(), &dropin), false)),
        };
    }
    Ok((choice, false))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
