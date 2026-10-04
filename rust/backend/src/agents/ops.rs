//! accounts.list: the accounts this daemon's home holds
//! now, discovered fresh on every call.

use crate::handlers::HandlerOutput;
use anyhow::Result;
use sot_protocol::op;
use sot_protocol::Frame;

/// `accounts.list` (accounts brief, v0.6.0): discover, fresh, every
/// account this daemon's own home has right now — no declaration to
/// read, no cache. `None` home (unresolvable `$HOME`/`%USERPROFILE%`)
/// answers the empty list rather than an error: nothing to discover is
/// an honest, non-fatal answer, and `workspace.create`'s own account
/// check hits the same "no home" case as its own refusal if it matters
/// there.
pub async fn handle_accounts_list(req_id: u64, _payload_json: serde_json::Value) -> Result<HandlerOutput> {
    use sot_protocol::{AccountEntry, AccountsListRes};
    let accounts = crate::accounts::account_home()
        .map(|home| crate::accounts::discover_accounts(&home))
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
