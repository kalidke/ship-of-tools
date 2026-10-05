//! The one place a suite builds a command for `sotd`: it inherits no `SOT_` variable the runner holds, so a daemon
//! reads only the variables its test sets. `sotd_exe` is private, so no suite can spawn the binary any other way.

use std::path::PathBuf;
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
    let mut cmd = Command::new(sotd_exe());
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().to_ascii_uppercase().starts_with("SOT_") {
            cmd.env_remove(name);
        }
    }
    cmd
}
