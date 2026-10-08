# rust/log/src/supervisor/authority: the SOSV lane's server side (capsule)

The authority answers status, query and command requests on the supervisor lane. `mod.rs` holds the state those requests
read and change and how a command is admitted; `lane.rs` holds the connections and the per-tick service.
Part of the capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: `AuthorityState`, `CommandEffect`, `StopRequested`, the reply mapping and `self_pid_and_created`
- `lane.rs`: `Conn`, `PendingClose`, `LaneCtx`, `service_lane` and `handle_lane_bytes`

## Start here
`AuthorityState::handle_command` in `mod.rs` for what a command does; `service_lane` in `lane.rs` for the wire loop.

## Rules
- A replayed operation id resolves against the journal before voyage fencing (`AuthorityState::handle_command`).
- At most `LANE_EVENT_QUOTA` events are serviced per tick (`service_lane`).
- A refusal or stop reply closes only after `Sent` plus a flush grace (`PendingClose`).
- Stop severity only accumulates (`handle_lane_bytes`).
- A `journal::begin` that fails after its rename took is read back: a `.active` record with this digest is the admitted record, with its own voyage, epoch and aside; nothing is minted again (`begin_or_readback`).
- A Stop is honored even when storage exhaustion keeps its record from being written: it answers Stopping and the authority exits clean (`stop_effect`).
