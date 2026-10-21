# rust/backend/src/daemon_log: the daemon's log (module)

The log of record is `<state>/sotd.log`, a private `0600` file that rolls at 64 MiB; `TeeWriter` mirrors each line to
stdout too unless stdout is a regular file (a launcher's per-start log, which would grow without a bound). In that case
`init` writes one stderr line, `sotd: stdout is a file; log lines go to <path>`, so a launcher that captured stderr into
the file it names tells the truth about where the daemon's lines are.

## Files
- `mod.rs`: `init` (the subscriber and the stderr line), `TeeWriter`, `LogFile` and `roll_log`, `open_private_log_file`, `copies_to_stdout` and `stdout_is_file`
- `log_tests.rs`: unit tests: the masking, the roll, and where a regular-file stdout and a missing log file send the lines
