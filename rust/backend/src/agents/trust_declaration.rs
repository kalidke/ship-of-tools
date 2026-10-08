//! The typed user-level trust declaration and its offline publication owner.
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const USAGE: &str = "Usage: sotd trust declare <absolute-prefix>";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Deserialize, Serialize)]
struct Trust {
    #[serde(default)]
    root_prefix: Option<String>,
}
#[derive(Default, Deserialize, Serialize)]
struct Declaration {
    #[serde(default)]
    trust: Option<Trust>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeclarationOutcome {
    Declared,
    Kept,
}

pub(crate) fn declaration_file() -> PathBuf {
    crate::rows::store::app_config_dir().join("settings.toml")
}

/// Parse at each spawn. Unknown non-trust settings belong to their own schema.
pub(crate) fn read_trust_declaration(path: &Path) -> Result<Option<PathBuf>, String> {
    let Some(text) = read_document(path)? else {
        return Ok(None);
    };
    let declaration: Declaration = toml::from_str(&text).map_err(|e| at(path, e))?;
    Ok(declaration
        .trust
        .and_then(|t| t.root_prefix)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from))
}

fn at(path: &Path, error: impl std::fmt::Display) -> String {
    format!("{}: {error}", path.display())
}

fn read_document(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(at(path, e)),
    }
}

fn validate_prefix(prefix: &Path) -> Result<(), String> {
    if !prefix.is_absolute()
        || !prefix
            .components()
            .any(|c| matches!(c, Component::Normal(_)))
        || prefix
            .components()
            .any(|c| matches!(c, Component::ParentDir) || c.as_os_str() == "..")
    {
        return Err(
            "declaration requires an absolute non-root prefix without parent components".into(),
        );
    }
    Ok(())
}

/// Keep any existing trust answer verbatim; append only to valid settings without one.
pub(crate) fn declare_trust(path: &Path, prefix: &Path) -> Result<DeclarationOutcome, String> {
    declare_with(path, prefix, || {})
}

fn declare_with(
    path: &Path,
    prefix: &Path,
    before_publish: impl FnOnce(),
) -> Result<DeclarationOutcome, String> {
    validate_prefix(prefix).map_err(|e| at(path, e))?;
    let existing = read_document(path)?;
    if let Some(text) = &existing {
        let doc: toml::Table = toml::from_str(text).map_err(|e| at(path, e))?;
        if doc.contains_key("trust") {
            return Ok(DeclarationOutcome::Kept);
        }
    }
    let root_prefix = prefix
        .to_str()
        .ok_or_else(|| at(path, "prefix is not UTF-8"))?;
    let declaration = Declaration {
        trust: Some(Trust {
            root_prefix: Some(root_prefix.to_owned()),
        }),
    };
    let table = toml::to_string(&declaration).map_err(|e| at(path, e))?;
    let mut bytes = existing.as_deref().unwrap_or_default().as_bytes().to_vec();
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(table.as_bytes());
    let parent = path
        .parent()
        .ok_or_else(|| at(path, "settings file has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|e| at(path, e))?;
    if existing.is_some() {
        crate::durable::write(path, &bytes).map_err(|e| at(path, e))?;
        Ok(DeclarationOutcome::Declared)
    } else {
        create_declaration(path, &bytes, before_publish)
    }
}

fn create_declaration(
    path: &Path,
    bytes: &[u8],
    before_publish: impl FnOnce(),
) -> Result<DeclarationOutcome, String> {
    let parent = path.parent().expect("validated declaration parent");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".trust-{}-{sequence}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| at(path, e))?;
    let result = (|| {
        file.write_all(bytes).map_err(|e| at(path, e))?;
        file.sync_all().map_err(|e| at(path, e))?;
        drop(file);
        before_publish();
        match sot_log::host::publish_noreplace(&temporary, path) {
            Ok(()) => Ok(DeclarationOutcome::Declared),
            Err(sot_log::Error::Io(e)) if e.kind() == ErrorKind::AlreadyExists => {
                Ok(DeclarationOutcome::Kept)
            }
            Err(e) => Err(at(path, e)),
        }
    })();
    // Only our exclusive temporary file may be removed, even when a competing destination won.
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(at(path, e)),
    }
    result
}

pub(crate) fn run(args: &[String]) -> anyhow::Result<DeclarationOutcome> {
    if args.len() != 2 || args[0] != "declare" {
        anyhow::bail!("{USAGE}");
    }
    let path = declaration_file();
    declare_trust(&path, Path::new(&args[1])).map_err(anyhow::Error::msg)
}

#[cfg(test)]
#[path = "trust_declaration_tests.rs"]
mod tests;
