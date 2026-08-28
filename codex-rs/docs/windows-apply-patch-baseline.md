# Windows `apply_patch` instrumentation baseline

This document records the P0 observation points for a later Windows `apply_patch`
optimization. It does not describe or introduce a performance change.

## Source baseline

- Base commit: `668e27de461903f869b00555d810a40f74c0b9a5`
- Target: native Windows executor with filesystem sandboxing enabled
- Fixture: one small file in one workspace, exercised as create, update, then delete
- Measurement status: native Windows sandbox execution was not available from the
  environment used to prepare this change, so no timing or request counts are
  fabricated here. Run the fixture on native Windows and record the emitted
  tracing and sandbox-log events below before comparing an optimization.

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

For a sandboxed local filesystem operation the observed code path is:

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
path and does not use that credential-preparation refresh call. Baseline results
must therefore record the Windows sandbox level instead of assuming every
Windows helper invocation performs a setup refresh.

## Native Windows baseline table

Fill this from one consecutive create/update/delete fixture on the same native
Windows workspace. Count helper starts from the tracing completion events and
setup refreshes from the sandbox log completion events.

| operation | total `apply_patch` time | fs requests | fs-helper starts | setup refreshes | setup-refresh total |
| --- | ---: | ---: | ---: | ---: | ---: |
| create | not measured | not measured | not measured | not measured | not measured |
| update | not measured | not measured | not measured | not measured | not measured |
| delete | not measured | not measured | not measured | not measured | not measured |

Also retain the per-operation filesystem breakdown (`fs/readFile`,
`fs/writeFile`, and so on), because request count is the primary comparison
metric for the next change.

## Hypothesis for the next PR

If native Windows measurements show approximately one helper start per
filesystem request and, on the elevated backend, one setup refresh per helper
start, then repeated helper/setup preparation remains a candidate dominant cost.
The next PR should test that hypothesis against the measured counts and timing
before introducing batching, helper reuse, refresh caching, or any other
optimization.
