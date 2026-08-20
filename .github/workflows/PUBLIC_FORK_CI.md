# Public fork CI runner profile

`openai/codex` uses a mixture of standard GitHub-hosted runners, GitHub larger
runners, and repository-specific runner groups. A plain public fork does not
have access to the latter two classes, so this fork selects the runner topology
with the repository variable `CODEX_CI_RUNNER_PROFILE`.

## Profiles

- **Public/default**: leave `CODEX_CI_RUNNER_PROFILE` unset (or set it to any
  value other than `upstream`). `blocking-ci` and `postmerge-ci` route the jobs
  that depend on private runner topology through `public-ci.yml`, which only
  uses standard GitHub-hosted runner labels. Direct `pull_request` runs of
  `v8-canary.yml` remain metadata-only even when V8 changes are detected.
- **Upstream/enterprise**: set `CODEX_CI_RUNNER_PROFILE=upstream`. The entrypoint
  workflows call the upstream `bazel.yml`, `rust-ci.yml`, `sdk.yml`,
  `rust-ci-full.yml`, and `v8-canary.yml` graph. Direct pull requests also retain
  the upstream V8 canary matrix.

The default is intentionally fail-safe for a public repository: a fork cannot
opt itself into an organization's self-hosted runners merely by changing a
workflow file in a pull request, because repository variables come from the
base repository configuration.

## Upstream-only assumptions found in CI

| Dependency | Upstream usage | Public behavior |
| --- | --- | --- |
| Runner group `${repository}-runners` with `${repository}-linux-x64`, `-linux-arm64`, `-windows-x64`, and `-windows-arm64` labels | Bazel Windows jobs, Rust CI Windows jobs, SDK jobs, full Cargo/nextest matrix | Replaced by standard `ubuntu-24.04`, `ubuntu-24.04-arm`, `windows-latest`, and `windows-11-arm` jobs in `public-ci.yml` |
| `macos-15-xlarge` | Bazel, Rust CI/full CI, and V8 canary | Replaced by standard arm64 `macos-15` where public cross-platform signal is needed; direct PR V8 canary jobs are skipped in the public profile |
| `environment: bazel` | Bazel-backed upstream jobs | Not required by the public compatibility workflow; retained unchanged in upstream workflows |
| `BUILDBUDDY_API_KEY` | Remote cache/execution and the OpenAI BuildBuddy tenant | Optional. Existing Bazel wrappers already remove remote-execution CI configs and use local Bazel when the secret is absent. Trusted-upstream checks prevent a fork PR from selecting the OpenAI tenant. |
| Release/signing/organization automation secrets and environments | Release, CLA, issue automation and related workflows | Outside this compatibility layer. They remain upstream-specific and are not made runnable from untrusted public-fork code. |

## Coverage trade-off

The public profile keeps the portable policy checks, runs the upstream fast Rust
checks, argument-comment lint on Linux/macOS/Windows, full Bazel tests on the two
Linux x64 targets, cross-platform Cargo checks on standard macOS/Windows, and SDK
build/lint/tests that do not require an upstream runner. Postmerge additionally
adds standard GitHub-hosted Linux ARM64 and Windows ARM64 smoke checks.

It intentionally does not try to reproduce OpenAI's RBE-backed Windows Bazel
shards, larger-runner capacity, full nextest sharding, or V8 canary matrix on
smaller public runners. Direct PR V8 canary runs keep the cheap metadata/change
detection job but gate the expensive build jobs on `CODEX_CI_RUNNER_PROFILE=upstream`.
Those jobs remain available without modification to their matrix by opting into
the `upstream` profile.

## Upstream maintenance

Keep the compatibility layer concentrated at the two entrypoints
(`blocking-ci.yml` and `postmerge-ci.yml`) plus `public-ci.yml`. `v8-canary.yml`
has one deliberately small compatibility gate on its expensive direct-PR jobs;
keep that delta limited to the profile condition when merging upstream changes.
Avoid copying other changes into upstream child workflows unless a public-runner
compatibility bug requires it. This keeps ordinary merges from `openai/codex`
focused on a small, stable fork delta.
