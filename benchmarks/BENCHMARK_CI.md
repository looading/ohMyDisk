# Manual native benchmark workflow

The `manual-native-benchmark` workflow in
`.github/workflows/benchmark.yml` runs only through `workflow_dispatch`. It has
no push, pull-request or schedule trigger. Once the workflow is present on the
default branch, select **Actions → manual-native-benchmark → Run workflow** and
choose the branch or tag to measure. GitHub requires the workflow file to be on
the default branch for manual dispatch to be available.
See [GitHub's manual-trigger documentation](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow).

The native matrix matches the build workflow:

| Runner | Rust target | Result artifact |
| --- | --- | --- |
| `windows-2022` | `x86_64-pc-windows-msvc` | `benchmark-windows-x86_64-ATTEMPT` |
| `macos-15` | `aarch64-apple-darwin` | `benchmark-macos-aarch64-ATTEMPT` |
| `macos-15-intel` | `x86_64-apple-darwin` | `benchmark-macos-x86_64-ATTEMPT` |

These labels identify GitHub-hosted native architectures, rather than running
cross-compiled binaries through emulation. Hosted images and available hardware
can change, so compare measurements from the same platform and inspect the
recorded image version. Runner labels are documented in
[GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

## Configuration and reproducibility

Rust is pinned to `1.94.1`; Python uses the `3.13` release series. Both release
executables are built from the selected commit with `cargo build --locked` and
the repository's release profile. Action dependencies use the same pinned
commit SHAs as the build workflow. The metadata and harness reports record the
source commit, run/attempt, native target, actual Python/Rust versions, available
runner image identifiers, binary SHA-256 values, fixture parameters and raw
numeric trials. Repeating a run reconstructs the same generated data, but is not
a guarantee of identical cloud-runner timing.

Dispatch inputs are limited to these choices:

| Input | Default | Choices |
| --- | --- | --- |
| `fixture_profile` | `standard` | `standard`, `smoke` |
| `iterations` | `5` | `5`, `7`, `10` |

Each binary/mode receives one excluded warmup. All cases use fixed seed
`20261028`. Timings use warm filesystem caches; the workflow does not clear OS
caches. Profiles are fixed in the workflow:

| Fixture | Standard | Smoke |
| --- | --- | --- |
| Core small files | 100,000 × 128 bytes | 1,000 × 128 bytes |
| Core wide directory | 50,000 × 32 bytes | 500 × 32 bytes |
| Core mixed | 10,000 requested files plus link fixtures | 100 requested files plus link fixtures |
| Core deep tree | 64 levels × 64 files | 16 levels × 4 files |
| Service small files | 100,000 × 128 bytes | 1,000 × 128 bytes |

The bounded depth keeps the same fixture shape on Windows and macOS without
requiring a machine-wide path-length configuration change. Three separate
cancellation probes per core binary/mode supplement the complete scans. Fixture
creation, inventories, service startup and teardown stay outside scan/query
timings. The core and service harnesses run sequentially within each VM. Cargo
cache save occurs after measurements. The cache key matches the native build
workflow's OS, architecture, actual compiler version, lockfile/config and Rust
source hashes, allowing the benchmark to reuse its already compiled dependencies.
Cache restoration and all required rebuilding remain outside measured timings.

## What the comparisons mean

The core harness supplies the **same current release runner** as candidate and
reference. Candidate uses `default`; reference uses
`--portable-enumeration`. Execution order alternates per iteration. On macOS,
this isolates native bulk enumeration from the current traversal, aggregation
and query implementations. On Windows, both modes currently use the portable
enumerator, so the comparison establishes semantic parity and provides a useful
control for measurement noise. Ratios are candidate median divided by portable
median; a ratio below 1 means a lower measured value.

This comparison does not isolate every change since v0.1.0. Both modes use the
current traversal and `QueryIndex`; its one-time construction is measured in
`index_build_ms`, outside `scan_ms` and the scanner's process peak-RSS sample.
`first_children_ms` and `first_descendants_ms` measure indexed queries after that
construction. A preserved v0.1.0 diagnostic runner uses the original uncached
query path, so its query timings have a different setup cost. See
[SCAN_BENCH.md](SCAN_BENCH.md) for local old/new comparisons and precise metric
definitions.

The service harness measures the current release application only. It reports
scan acceptance/completion, persistence/index stages when supplied by the
service, first and warm queries, stored/original payload bytes, database sizes
and sampled RSS. `--historical-restart` additionally restarts the service
separately for cold historical children and descendants queries against the
saved scan, then repeats each query warm and verifies the same responses.
This makes the application cache cold while OS filesystem caches remain warm;
it does not rescan the fixture. The harness starts explicitly supervised
foreground services, including Windows; it does not rely on detached service startup. No
historical release binary is downloaded or executed. To compare a separately
verified preserved application, use the local `--reference-binary` flow in
[SERVICE_BENCH.md](SERVICE_BENCH.md).

## Correctness gates and artifacts

Execution errors and correctness failures fail the job. Core validation checks
independent inventory, totals, completeness, stable snapshot digests and fixture
stability. Service validation checks generated counts/bytes/completeness and
identical repeated and historical query responses. Cancellation latency is reported only when
cancellation is observed; finishing before the cancellation request is not a
timing failure. No measured median, p95, ratio, memory or throughput threshold
can fail a run. Operational build, harness and job timeouts still reject
incomplete executions.

Every matrix job writes a Markdown summary to the Actions run page through
`GITHUB_STEP_SUMMARY`, as described in
[GitHub's summary documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-commands#adding-a-job-summary).
It also uploads these files, including after a validation failure:

- `scan.json`: sanitized raw numeric core trials, summaries and validation state.
- `service.json`: sanitized raw numeric service trials and summaries, when the
  harness completed successfully.
- `metadata.json`: commit, toolchain, image and fixture configuration.
- `status.json`: per-harness execution and validation status.
- `summary.md`: the same readable summary shown in Actions.

Only these JSON/Markdown outputs are uploaded. No paths, user file names, raw
stdout/stderr, SQLite databases, snapshots or fixture directories are included.
The workflow accepts no scan-root input and always uses generated temporary
data. Harness failures are reduced to fixed labels and numeric exit codes.

Artifacts are retained for 14 days, subject to repository retention policy.
Download them from the run's **Artifacts** area; GitHub's
[artifact documentation](https://docs.github.com/en/actions/tutorials/store-and-share-data)
describes downloads and retention. A failed build can produce only a summary
with missing measurements; it does not produce a passing benchmark report.
