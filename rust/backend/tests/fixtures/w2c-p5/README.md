# W2(c) P5: hosted premise receipt

This revised test-only fixture belongs only to `tmp/ci-w2c-p5-2`, starting at
the first premise attempt `2d9d04221`, whose production base is
`8c5f0d24f733f563cc8b30d292fe37f8d2ab640d`. It changes no production behavior.
Run it only through the scratch `w2c-p5` job in `rust.yml`, on disposable
GitHub-hosted Ubuntu 22.04 and 24.04 runners. Do not run it on a lab machine.
The entry requires the dedicated branch, GitHub-hosted runner metadata, an
unprivileged process and the matching Ubuntu image before creating anything.
The first hosted attempt measured no P5 cases: its minimum-version manager
could not create its control group, and the current-version manager exited
without captured diagnostics. P5 remains not checked until the revised jobs run.

## Delegated hosted entry and diagnostics

Only the hosted P5 workflow step provisions a transient service through
`sudo systemd-run`, with `Delegate=yes` and `User=runner`. It runs the same
unprivileged probe with a 120-second command bound and ten-second forced-exit
grace. The service has a 145-second runtime limit and ten-second stop limit;
the provisioning wait has a 175-second outer bound. Its cgroup owns every
descendant even if the probe cannot complete normal recorded-child cleanup.
No such provisioner or private manager is run on a lab machine.

Before manager startup, the probe records its cgroup and actually creates and
removes one empty child cgroup in its delegated subtree. Before reporting a
readiness failure, it saves `startup` in the receipt: that observation, the
recorded manager child's cgroup when still readable, its exit status, and the
last 40 manager log lines. Paths and private identifiers are sanitized. A
running manager has a null exit status; a gone child has a named unavailable
cgroup observation. A failed creation records its exception rather than
guessing about permission. An unknown unified hierarchy is reported as an
unmeasured delegation result, with no alternate bootstrap attempted.

`python3 -B rust/backend/tests/fixtures/w2c-p5/diagnostic_tests.py -v` runs
local behavior controls over temporary ordinary directories and doubles.
It starts no bus, manager, service or process. These controls verify saved
failure diagnostics and sanitization; they establish no systemd premise.

## Private fixture and measured cases

Each run creates one absolute temporary root containing its own homes, unit
directory, runtime directory and D-Bus configuration. The bus has no service
activation directories. Each busctl query explicitly names that bus; the
fixture verifies that its systemd owner is the manager child it created.
The direct manager socket that systemctl may select is checked for that same
recorded child and OS account. Neither command can select the account's bus.
Only the empty default target is started. Relay services are loaded and
inspected, never started. Each has an execution marker as a negative control.
The fixture does not add unit references or pin inactive relay instances: if
`LoadUnit` followed by the required queries loses an instance, P5-query fails.

The receipt preserves every actual JSON envelope, signature, tuple nesting,
execution-history field and legacy command display, with private identifiers
and temporary paths replaced by placeholders. The generated command input is
the command at the recorded base. Experimental alternate executable, ignoring
error policy, merged arguments, changed command, valid extra command and
invalid extra command cases use the same private manager. Neither a hand-built
JSON response nor a source-text scan supplies schema evidence.

Timeout controls stop only the retained manager child and observe its stopped
state before each real query. `--timeout=250ms` must produce a named timeout
for both manager methods and both properties; an external ten-second hang
guard is a failure, never a successful timeout observation. Cleanup resumes,
terminates and, if necessary, kills only children created and retained by this
fixture, with bounded reaps, then removes its own absolute temporary root.

The `w2c-p5 (systemd-249)` job runs `bash
rust/backend/tests/fixtures/w2c-p5/proofs.sh --label systemd-249` on
`ubuntu-22.04` inside the delegated service. The `w2c-p5 (systemd-current)` job runs the same command with
`--label systemd-current` on `ubuntu-24.04`. Each job always prints the receipt
from `dev/output/w2c-p5/<label>/receipt.json`, including on a probe failure.

## Required hosted receipts

For **each** label, the job must print these exact lines in this order (replace
`<label>` with `systemd-249` or `systemd-current`):

```text
P5 <label> versions-recorded PASS
P5 <label> startup-diagnostic ready SAVED
P5 <label> delegated-cgroup writable PASS
P5 <label> private-bus-and-manager-owned PASS
P5 <label> generated inactive-and-identity-measured PASS
P5 <label> alternate-executable inactive-and-identity-measured PASS
P5 <label> ignore-errors inactive-and-identity-measured PASS
P5 <label> merged-argument inactive-and-identity-measured PASS
P5 <label> changed-command inactive-and-identity-measured PASS
P5 <label> extra-command inactive-and-identity-measured PASS
P5 <label> bad-setting inactive-and-identity-measured PASS
P5 <label> timeout-LoadUnit bounded PASS
P5 <label> timeout-GetUnit bounded PASS
P5 <label> timeout-LoadState bounded PASS
P5 <label> timeout-ExecStart bounded PASS
P5 <label> no-service-executed PASS
P5 <label> owned-children-reaped PASS
P5 <label> COMPLETE PASS
```

A readiness failure must instead print `P5 <label> startup-diagnostic
readiness-failure SAVED` before its failure line and leave the diagnostic in
the printed receipt. A cleanup PASS alone still measures no P5 case.

The minimum job must additionally record actual systemd and busctl version
249. The merged-argument receipt's `old_accept` records whether the actual
display collides; neither outcome is predeclared a red regression. The other
identity regressions require measured old-verifier acceptance and independently
loadable units. A failure or missing receipt leaves P5 unsettled. These lines
are required future output, not a claim that either job has run.

The API and tool documentation inform the probe's requests, not its result:
[systemd D-Bus interface](https://github.com/systemd/systemd/blob/v249/man/org.freedesktop.systemd1.xml)
and [busctl options](https://github.com/systemd/systemd/blob/v249/man/busctl.xml).
