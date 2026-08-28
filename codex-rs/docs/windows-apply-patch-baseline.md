# Windows `apply_patch` instrumentation baseline

This document records the P0 observation points and native-Windows baseline for a
later Windows `apply_patch` optimization. It does not describe or introduce a
performance change.

## Source baseline

- Base commit: `668e27de461903f869b00555d810a40f74c0b9a5`
- Instrumented build commit: `0055f2cd7ad6391c3cc9ddf73622622aa2e4eb48`
- Baseline build run: `33192154590`
- Measurement run: `33198228096`
- Target: native Windows executor with filesystem sandboxing enabled
- Windows sandbox level: `elevated`
- Runner OS: Microsoft Windows Server 2025, `10.0.26100`
- Runner image: `windows-2025-vs2026`, version `20260824.214.3`
- Fixture: one small file in one workspace, exercised as create, update, then delete
- Warm-up: one sandboxed metadata request before the measured fixture, so initial
  account/setup provisioning is excluded from the create/update/delete rows

Absolute hosted-runner timing is not treated as a stable benchmark. Request counts,
helper-start counts, setup-refresh counts, and latency composition are the primary
baseline signals.

## Observation points

`ApplyPatchRuntime::run` already records the total wall time in
`ExecToolCallOutput.duration`. Each sandboxed filesystem request now emits a
`filesystem sandbox helper invocation started` tracing event and a matching
completion event with the existing protocol operation name, success flag, and
elapsed milliseconds. Windows elevated sandbox setup refresh already logs the
helper spawn; its credential-preparation call now also logs completion, success,
and elapsed milliseconds.

The filesystem helper operation names are the protocol names, for example
`fs/readFile`, `fs/writeFile`, `fs/createDirectory`, `fs/getMetadata`, and
`fs/remove`.

## Current call path

For a sandboxed local filesystem operation the measured code path is:

```text
apply_patch
  -> ExecutorFileSystem operation
     -> FileSystemSandboxRunner
        -> one fs helper process for that request
           -> Windows sandbox backend
```

On the elevated Windows sandbox backend, helper spawn preparation calls
`require_logon_sandbox_creds`, which always performs the non-elevated setup
refresh for the current roots:

```text
fs helper spawn
  -> elevated Windows sandbox spawn preparation
     -> require_logon_sandbox_creds
        -> setup refresh helper
```

The restricted-token legacy backend follows a different token/ACL preparation
path and does not use that credential-preparation refresh call. The baseline
therefore records the Windows sandbox level rather than generalizing this result
to every Windows sandbox backend.

## Native Windows baseline

| operation | total `apply_patch` ms | fs requests | fs-helper starts | fs-helper total ms | setup refreshes | setup-refresh total ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| create | 652.230 | 3 | 3 | 609.204 | 3 | 247.981 |
| update | 736.365 | 3 | 3 | 691.718 | 3 | 312.030 |
| delete | 911.374 | 4 | 4 | 848.129 | 4 | 366.457 |

Filesystem operation breakdown:

| operation | filesystem requests |
| --- | --- |
| create | `fs/getMetadata` x1, `fs/readFile` x1, `fs/writeFile` x1 |
| update | `fs/getMetadata` x1, `fs/readFile` x1, `fs/writeFile` x1 |
| delete | `fs/getMetadata` x2, `fs/readFile` x1, `fs/remove` x1 |

The measured count relationship is exact for this elevated-backend fixture:

```text
filesystem requests == fs-helper starts == setup refreshes
```

The filesystem-helper path accounts for approximately 93.4% of create wall time,
93.9% of update wall time, and 93.1% of delete wall time. Setup refresh alone
accounts for approximately 38.0%, 42.4%, and 40.2% of total wall time respectively,
or about 40.7-45.1% of the filesystem-helper time. Setup refresh is therefore a
large repeated component, while the broader per-request helper/wrapper/IPC path
is the dominant latency envelope.

Do not add filesystem-helper total and setup-refresh total: setup refresh occurs
inside each elevated filesystem-helper invocation.

## Hypothesis for the next PR

The baseline confirms one helper start per filesystem request and one setup
refresh per helper start for the measured elevated Windows sandbox path. The next
optimization should therefore reduce repeated per-request sandbox/helper setup
work while preserving filesystem, approval, retry, symlink, and sandbox semantics.
The first comparison should be request/helper/setup call counts and latency
composition against this baseline, rather than absolute hosted-runner timing.
