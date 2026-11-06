# rust/chat: chat (contract)

The project chat's policy. This stage holds one piece, normalization: Slack's raw message JSON into the wire's one
record (`sot_protocol::chat::ChatRecord`). The crate (`sot-chat`) depends on `sot-protocol`, `serde_json` and nothing
else; nothing links it but the workspace (`sotd` does not yet). It touches no network, owns no runtime state, and has
no Slack connection, no delivery and no channel mapping.

## Guarantees
1. **Every message shape gives a result, never a panic or a silent drop.** `normalize(ctx, channel, raw)` returns
   `New` (a message), `Edited` (a changed message under its original identity, so a caller can tell it from new work),
   `Deleted` (the identity to remove), `Unsupported` (a subtype that is not a message: joins, tombstones, ...) or
   `Unrepresentable` (not an object, a bad workspace, channel or timestamp, a wrong-typed `subtype` or edit body): the
   caller treats the last as a gap. A message with a valid `ts` whose other fields cannot be read becomes a record with
   `omitted` set and no content, not a dropped message. Limit: the author's display name is the event's own profile
   name or empty; resolving a member's name needs the API, which is a later stage.
2. **A forged marker never makes an own-app post.** Own-app is decided only by the bot identity the caller supplies
   (`Context.own_bot_id`, `own_user_id`; none supplied, no post is own). Only an own post has its origin recovered, in
   order from `metadata`, the `username` suffix ` (<kind> via Ship of Tools)`, then the text prefix, giving `Drawer`
   (person) or `Row`; an own post with no marker, any other bot, and a member who types the marker, stay `Bot`
   and `Member` with their text untouched.
3. **A record is bounded, and a cut is marked.** 32 KiB of encoded JSON, 8 KiB of text, eight file summaries (the rest
   counted in `files_omitted`, as is any file entry with no valid id), 64 ASCII-byte ids, 160 / 256 / 128 bytes for an
   author name / filename / MIME. A cut falls on a character boundary and sets `truncated` (which covers text, names,
   filename and MIME); the record's size is held by halving the text until it fits. Names lose control characters. A
   file stub (`file_access: check_file_info`) keeps its id only. A private URL is never copied; a thumbnail is a flag.
   Limit: `truncated` does not say which field was cut.
4. **An attachment-only message is a record with empty text and its files.** Nothing needs a caption.

The timestamp rule (exactly `<seconds>.<six digits>`) is wire's `Ts`; this crate builds only `Ts` values, so a record
never holds another form.

## Owns
`normalize.rs`: `normalize`, `Context`, `Normalized`, `Unrepresentable` and the bound constants.

## Calls in
`sot_protocol::chat::{ChatRecord, ChatAuthor, ChatFile, Origin, Ts}`, `serde_json::Value`.

## Does not
Open a connection, hold a credential, call Slack, file mail, map channels to rows, or resolve a name. Define the
record's representation (wire's).

## Tests
Paths are under `rust/chat/src/`.

| # | guarantee | test |
|---|---|---|
| 1 | result for every shape | `normalize_tests.rs`: `normalize_members_bots_edits_deletes_and_file_stubs`, `hostile_json_gives_an_explicit_result` |
| 2 | no forged own post | `normalize_tests.rs`: `normalize_members_bots_edits_deletes_and_file_stubs` (member prefix, member username, other bot, no identity supplied) |
| 3 | bounds | `normalize_tests.rs`: `text_is_cut_on_a_character_boundary_and_marked`, `names_filenames_and_mime_are_cut_and_marked`, `only_eight_files_are_summarized_and_the_rest_counted`, `an_id_over_64_bytes_or_not_ascii_is_not_carried`, `a_record_never_exceeds_32_kib_of_json`, `a_normalized_record_round_trips_through_the_wire` |
| 4 | attachment only | `normalize_tests.rs`: `an_attachment_only_message_keeps_its_files_and_has_no_text` |

## Files
- `Cargo.toml`: the manifest; depends on wire.
- `src/lib.rs`: the module and its re-exports.
- `src/normalize.rs`: the normalization.

## Start here
`src/normalize.rs`, `normalize`.
