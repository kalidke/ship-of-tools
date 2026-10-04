# rust/log/src/capsule/producer: what the leg runs the agent on (capsule)

The writer loop never touches an OS primitive directly: it drives one `Producer` through nine verbs (spawn, take
output, input, resize, wait, exit status, terminate domain, domain empty, close output side). A Unix pty producer and a
Windows ConPTY producer implement them, and a small detector answers the host's DA1 query. Part of the log subsystem;
charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the `Producer` trait, `ExitStatus` and `ParentLease`
- `host_handshake.rs`: `HostHandshake`, the byte-ordered DA1 query detector, and `DA1_REPLY`
- `pty/`: the Unix producer (`PtyProducer`)
- `conpty/`: the Windows producer (`ConptyProducer`) and the owned ConPTY primitives

## Start here
`Producer` in `mod.rs` for what the writer loop may ask of any producer; then the platform folder for how it is answered.

## Rules
- The kill domain is the producer's: a setsid process group plus `PR_SET_PDEATHSIG` on Linux (`PtyProducer::spawn`), a
  kill-on-close job on Windows (`AnonymousJob`).
- `HostHandshake::feed` counts DA1 queries across any chunk split; the writer loop answers only the first match of a run
  (`dsr_answered` in `capsule/writer_loop.rs`).
- `host_handshake` lives here, not under `conpty/`: `run` uses it on every platform and `conpty/` is Windows only.
