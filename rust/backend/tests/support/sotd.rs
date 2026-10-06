//! The one place a suite builds a command for `sotd`: the built binary (`sotd_command`), or a copy or link of it for a
//! suite that tests how the daemon was started (`sotd_command_at`). Either inherits no `SOT_` variable the runner
//! holds, so a daemon reads only the variables its test sets. The built binary's path (`sotd_exe`) is private here.

use std::path::{Path, PathBuf};
use std::process::Command;

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

/// The path of the `sotd` binary, for a suite that runs it through another program (a shell) and so passes a path,
/// not a command.
#[allow(dead_code)] // only some suites run `sotd` through a shell
pub fn sotd_program() -> PathBuf {
    sotd_exe()
}

/// A command for `sotd` with every inherited `SOT_` variable removed.
pub fn sotd_command() -> Command {
    sotd_command_at(&sotd_exe())
}

/// [`sotd_command`] for the binary at `program`, a copy of the built `sotd` or a link to one, for a suite that tests
/// how the daemon was started. The same `SOT_` scrub.
#[allow(dead_code)] // only some suites start `sotd` from another path
pub fn sotd_command_at(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().to_ascii_uppercase().starts_with("SOT_") {
            cmd.env_remove(name);
        }
    }
    cmd
}
