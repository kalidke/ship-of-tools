//! The options of `sotd install`: `--pack <folder>`, which the bootstrap adds, then the person's own, spelled as the
//! entry of this system spells them (scripts/CLAUDE.md, Installers: the option names never move). An option error is a
//! refusal (exit 2) in the one verdict line.

use std::path::PathBuf;

use super::verdict::Stop;

/// Where a Unix install's role comes from when it names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RoleFlag {
    /// `--local`: the window and the daemon here.
    Local,
    /// `--backend <alias>`: the window here, the daemon on the box at the ssh alias.
    Remote(String),
    /// `--be-only`: the daemon alone.
    BeOnly,
}

#[derive(Debug, Default, Clone)]
pub(super) struct Options {
    /// The folder the bootstrap verified and unpacked the pack into.
    pub pack: PathBuf,
    pub version: Option<String>,
    pub archive: Option<PathBuf>,
    pub prefix: Option<PathBuf>,
    pub hub: Option<String>,
    pub role: Option<RoleFlag>,
    pub no_service: bool,
    pub force_role_change: bool,
    pub restart_daemon: bool,
}

impl Options {
    /// Whether the person named what to install: a version or a pack.
    pub fn named(&self) -> bool {
        self.version.is_some() || self.archive.is_some()
    }
}

/// The options in `args` (what follows `sotd install`), in this system's spelling.
pub(super) fn parse(args: &[String]) -> Result<Options, Stop> {
    let mut o = Options::default();
    let mut rest = args.iter();
    let mut pack = None;
    while let Some(a) = rest.next() {
        if a == "--pack" {
            pack = Some(rest.next().ok_or_else(|| Stop::at("options", 2, "--pack needs a folder"))?);
            continue;
        }
        if cfg!(windows) { windows_option(&mut o, a, &mut rest)? } else { unix_option(&mut o, a, &mut rest)? }
    }
    o.pack = PathBuf::from(pack.ok_or_else(|| Stop::at("options", 2, "no --pack: run the installer (scripts/install.sh, scripts/install.ps1), which fetches and verifies the pack this runs from"))?);
    if o.version.is_some() && o.archive.is_some() {
        let both = if cfg!(windows) { "-Archive and -Version" } else { "--archive and --version" };
        return Err(Stop::at("options", 2, format!("{both} are exclusive: the folder names its own version")));
    }
    Ok(o)
}

fn value<'a>(name: &str, what: &str, rest: &mut impl Iterator<Item = &'a String>) -> Result<String, Stop> {
    match rest.next() {
        Some(v) if !v.is_empty() && !(cfg!(windows) && v.starts_with('-')) => Ok(v.clone()),
        _ => Err(Stop::at("options", 2, format!("{name} needs {what}"))),
    }
}

fn unix_option<'a>(o: &mut Options, a: &str, rest: &mut impl Iterator<Item = &'a String>) -> Result<(), Stop> {
    match a {
        "--local" => o.role = Some(RoleFlag::Local),
        "--backend" => o.role = Some(RoleFlag::Remote(value(a, "an ssh alias", rest)?)),
        "--be-only" => o.role = Some(RoleFlag::BeOnly),
        "--hub" => o.hub = Some(value(a, "an ssh alias", rest)?),
        "--version" => o.version = Some(value(a, "a value", rest)?),
        "--archive" => o.archive = Some(value(a, "a folder", rest)?.into()),
        "--prefix" => o.prefix = Some(value(a, "a folder", rest)?.into()),
        "--no-service" => o.no_service = true,
        "--force-role-change" => o.force_role_change = true,
        "--restart-daemon" => o.restart_daemon = true,
        other => return Err(Stop::at("options", 2, format!("unknown flag: {other}"))),
    }
    Ok(())
}

/// PowerShell's option names, matched without regard to case as PowerShell matches them.
fn windows_option<'a>(o: &mut Options, a: &str, rest: &mut impl Iterator<Item = &'a String>) -> Result<(), Stop> {
    match a.to_ascii_lowercase().as_str() {
        "-version" => o.version = Some(value(a, "a value", rest)?),
        "-archive" => o.archive = Some(value(a, "a value", rest)?.into()),
        "-hub" => o.hub = Some(value(a, "a value", rest)?),
        "-prefix" => o.prefix = Some(value(a, "a value", rest)?.into()),
        "-restartdaemon" => o.restart_daemon = true,
        _ => return Err(Stop::at("options", 2, format!("unknown option: {a}"))),
    }
    Ok(())
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;
