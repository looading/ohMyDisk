# Scan progress telemetry: paired overhead check

The scan-progress candidate was compared with the **pre-progress Auto-concurrency
baseline** at `a7b3abfb6efd5f8f6cbb91bd0843e15b9b708f17`. This avoids comparing with
the older worker-matrix executable, which predates Auto CPU detection. Both
release runners explicitly selected four workers and four filesystem permits.

Hardware: Apple M5 Pro, 18 logical / 18 physical CPUs, 48 GiB, macOS 26.6.2, APFS.
Rust/toolchain and other environment fields are retained in the raw report.

The baseline was exported with `git archive` into an owned temporary directory
and built there using `MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release
-p ohmydisk-platform --example scan-bench`, reusing a local Cargo target cache.
The candidate was built in the isolated scan-progress workspace. Both executables
were copied before timing and their hashes verified again afterward:

- Candidate: `261c87812e9a60ce63085ab6054067264c01c2c181b1880eeb55a88dcb27f948`.
- Reference: `e2dc021b2cd3230898024c628fc9a94d4be495245a4baeb13ed4e7b2b85c6aa7`.

Each role received two excluded warmups and seven measured scans on the same
owned fixture, with candidate/reference order alternating each iteration. No
cancellation probes or OS cache clearing were performed. Inventory warms metadata,
so this is a warm-cache experiment. This task's agents ran no builds or other
scans alongside timing; OS and other desktop activity remained uncontrolled.

## Measured results

Times are median / nearest-rank p95 in milliseconds. With seven samples, p95 is
the maximum sample. RSS is the process high-water mark at scan return, before
benchmark hashing, serialization and query/index diagnostics.

| Fixture | Candidate scan ms | Reference scan ms | Candidate / reference median | Paired-ratio median | Candidate / reference RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 100k small files / 100 directories | 346.46 / 351.96 | 344.42 / 346.75 | 1.0059 | 1.0027 | 72.39 / 72.62 |
| 5k directories × 2 files + 5k empty directories | 655.10 / 686.77 | 653.89 / 697.52 | 1.0018 | 0.9910 | 25.67 / 25.78 |

The median-of-run-times ratios changed by approximately +0.6% and +0.2%; the
same-trial paired ratios were +0.3% and -0.9%. These small differences are timing
noise in this sample, not evidence of an exact instrumentation cost or a speedup.
No substantial scan-time or RSS regression is visible in this run. This does not
establish real-home, cold-storage, Windows or end-to-end service/GUI performance.
`scan_ms` includes traversal, worker joining, native volume observations and
aggregation; it excludes service persistence/IPC, GUI rendering and job polling.

## Correctness and filesystem work

All **36 full/warmup scans** passed independent inventory and expected completion
checks, with one canonical structural fingerprint and one occupancy fingerprint
per fixture across both roles. Postflight inventories were unchanged.

| Fixture | Nodes / files / directories | Instrumented metadata / read-directory / volume observations |
| --- | ---: | ---: |
| 100k small files / 100 directories | 100101 / 100000 / 101 | 102 / 101 / 2 |
| 5k directories × 2 files + 5k empty directories | 20001 / 10000 / 10001 | 10002 / 10001 / 2 |

Those Platform-method counts were identical in every warmup and measured scan
for both roles. They count instrumented method calls, not all native syscalls.
This agrees with the code change updating counters from existing traversal events
instead of introducing another filesystem walk. Median delivered progress changes
were 4 versus 3 for small files and 5 versus 4 for directory-heavy work.

Raw serialized snapshot lengths were identical for small files (64,730,544 bytes).
Directory-heavy lengths ranged from 12,735,194 to 12,735,198 bytes, with equal
medians; raw output contains values excluded from the canonical fingerprint, so
byte-for-byte serialization is not a correctness requirement. Canonical structure,
logical totals and occupancy fingerprints matched.

The harness-owned fixtures and the exported baseline source checkout were cleaned.
Separate UI fixture/state and both frozen executables were preserved for native
acceptance. The [raw report](results/2026-11-02/scan-progress/overhead.json) contains
all timings, fingerprints, method counts, build/source provenance and hash checks.

## Reproduce the paired scan

Build the pre-progress reference from the exact commit above and the candidate
from the scan-progress source with the same release toolchain, then freeze both
executables before running:

```sh
python3 benchmarks/scan_bench.py \
  --binary <frozen-candidate> --reference-binary <frozen-reference> \
  --workers 4 --reference-workers 4 \
  --fixture small-files --small-files 100000 \
  --fixture directory-heavy --directory-count 5000 \
  --directory-files-per-dir 2 --empty-directories 5000 \
  --iterations 7 --warmups 2 --cancel-iterations 0 \
  --output benchmark-output/scan-progress-overhead.json
```

The harness creates and removes its own data. The candidate source-file hashes,
base commit and dirty-diff hash are retained in the report because the isolated
candidate had not been committed when these measurements were taken.
