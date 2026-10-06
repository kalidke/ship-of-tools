//! accounts.list (the accounts this daemon's home holds, discovered fresh on every call) and `sotd agent-exec`.

use crate::server::reply::HandlerOutput;
use anyhow::Result;
use sot_protocol::op;
use sot_protocol::Frame;
#[cfg(unix)]
use std::path::PathBuf;

/// `accounts.list` (accounts brief, v0.6.0): discover, fresh, every
/// account this daemon's own home has right now — no declaration to
/// read, no cache. `None` home (unresolvable `$HOME`/`%USERPROFILE%`)
/// answers the empty list rather than an error: nothing to discover is
/// an honest, non-fatal answer, and `workspace.create`'s own account
/// check hits the same "no home" case as its own refusal if it matters
/// there.
pub async fn handle_accounts_list(
    req_id: u64,
    _payload_json: serde_json::Value,
) -> Result<HandlerOutput> {
    use sot_protocol::{AccountEntry, AccountsListRes};
    let accounts = crate::agents::accounts::account_home()
        .map(|home| crate::agents::accounts::discover_accounts(&home))
        .unwrap_or_default();
    let res = AccountsListRes {
        accounts: accounts
            .into_iter()
            .map(|a| AccountEntry {
                name: a.name,
                kinds: a.kinds,
                logged_in: a.logged_in,
            })
            .collect(),
    };
    Ok(vec![(
        Frame::res(req_id, op::ACCOUNTS_LIST, serde_json::to_value(res)?),
        None,
    )])
}

/// ADR 0046 decision 4: the daemon's `agent_argv` is the ONE
/// owner of the launch recipe; `ccb` execs THROUGH this
/// rather than carrying its own copy of the scrub
/// list, the `~/.local/bin` PATH rule, and the flags. Unix
/// only (no wrapper anywhere on Windows — `claude_argv`'s own
/// Windows arm never resolves an absolute path, so there is
/// no equivalent "exec this resolved binary in place" step to
/// offer there); never in a producer path (the supervisor's
/// own `producer_argv` capture is untouched by this).
pub(crate) fn agent_exec() -> ! {
    #[cfg(not(unix))]
    {
        eprintln!("sotd agent-exec is not supported on this platform");
        crate::lifecycle::shutdown::exit(2);
    }
    #[cfg(unix)]
    {
        let kind = std::env::args().nth(2).unwrap_or_default();
        let flags: Vec<String> = std::env::args().skip(3).collect();
        let argv = match crate::agents::argv::agent_exec_argv(&kind, &flags) {
            Ok(argv) => argv,
            Err(msg) => {
                eprintln!("sotd agent-exec: {msg}");
                crate::lifecycle::shutdown::exit(2);
            }
        };
        for var in crate::agents::env::NESTING_ENV_VARS_TO_SCRUB {
            std::env::remove_var(var);
        }
        for (k, v) in crate::agents::env::agent_env(
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        ) {
            std::env::set_var(k, v);
        }
        use std::os::unix::process::CommandExt;
        #[allow(
            clippy::disallowed_methods,
            reason = "`sotd agent-exec` replaces its own process with the agent; no daemon runs here"
        )]
        let err = std::process::Command::new(&argv[0]).args(&argv[1..]).exec();
        eprintln!("sotd agent-exec: exec {:?} failed: {err}", argv[0]);
        crate::lifecycle::shutdown::exit(2);
    }
}
