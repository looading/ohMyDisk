# Automatic scan concurrency: 4 / 8 / CPU comparison

The desktop default uses `min(available_parallelism, 4)`. CPU count supplies a
bound; it does not establish the best filesystem concurrency. Manual selection
remains available for directory-heavy workloads. On this 18-core Mac, using all
18 workers increased CPU consumption substantially for small-file and mixed
scans, while its equal-weight wall-time advantage was small. This is a desktop
resource tradeoff, not a claim that four workers are optimal on every disk.

## Method and provenance

- Hardware: Apple M5 Pro, 18 physical / 18 logical CPUs, 48 GiB, APFS. The actual
  Rust `available_parallelism()` result was **18**, with no worker clamping.
- One release diagnostic executable was copied before measurement and remained
  unchanged. Each explicit configuration used 4, 8 or 18 directory workers and
  exactly that many filesystem permits; the Tokio executor had two threads.
  Batch limits were 128 entries / 256 KiB. These are isolated core scans, not a
  measurement of concurrent service scans sharing a global filesystem budget.
- Every case used the same owned generated fixture for all configurations.
  There were three warmups and twelve measured runs per configuration: all six
  permutations twice, balancing positions and within-trial directed neighbours.
  Warmups used cyclic rotations. Fixture creation, independent inventory,
  diagnostic serialization, fingerprints, index construction and queries were
  outside `scan_ms`. No OS cache clearing or user-directory scan was performed.
- Every one of the **270 full/warmup scans** passed independent inventory counts,
  logical bytes, expected boundary/completeness checks and the same structural
  fingerprint v1. Every case passed postflight metadata/inventory checks. The
  separate per-node occupancy fingerprints were also stable in every case.
  Occupancy drift alone is not a correctness failure: APFS allocation or external
  snapshots may change private size without changing file metadata.
- Median and nearest-rank p95 use twelve measured samples. Here p95 is the
  maximum sample, so it is not a stable estimate of production tail latency.
  Ratios below are medians of same-trial ratios, rather than ratios assembled
  from unrelated measurement groups. A lower ratio is faster.
- This task's agents ran no builds, tests or other scans during timing. This was a
  running desktop: other applications and OS work were not controlled. Aggregate
  `ps` CPU snapshots and system load averages are retained in the raw report;
  load average is not CPU utilization. Small wall-time differences should not
  be treated as stable wins. The results do not establish cold-disk, real-home,
  Windows, HDD, network-storage or low-core performance.

Measured base commit: `ea5a3769aff44a7fe594f96aa465753cf79e9f05`. The base had uncommitted
benchmark tooling changes; their source-diff hash and tool hashes are retained
in the raw report. The measured binary predates the Auto policy. A subsequent
source-only change replaced the runner's literal maximum with the shared core
constant (still 64), and made omitted `--workers` resolve Auto. All measured
trials explicitly selected 4, 8 or 18, so their selected behaviour is unchanged;
these source changes did not modify or rebuild the measured binary.

Measured binary SHA-256:
`180883c5f855106fb2799dbd3248f9c765ca67f707cfbf148c4a293b9126fe53`.

Raw evidence: [all trials and configuration](results/2026-11-02/worker-matrix/three-way.json),
[derived numeric summary](results/2026-11-02/worker-matrix/summary.json).
The summary retains the raw report SHA-256.

## Scan time

Times are median / p95 in milliseconds.

| Fixture | 4 workers | 8 workers | 18 workers | Paired 8 / 4 | Paired 18 / 4 | Paired 18 / 8 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 100k small files / 100 directories | 393.61 / 424.09 | 472.43 / 504.61 | 429.87 / 465.25 | 1.188 | 1.092 | 0.911 |
| One directory / 50k files | 214.37 / 224.91 | 217.36 / 242.21 | 214.77 / 226.92 | 1.024 | 0.990 | 1.020 |
| 128 levels × 64 files | 45.37 / 49.03 | 45.32 / 47.73 | 45.78 / 47.21 | 0.996 | 1.002 | 1.003 |
| 10k mixed files + hidden/link fixtures | 36.46 / 38.37 | 39.56 / 41.14 | 35.80 / 36.18 | 1.081 | 0.959 | 0.874 |
| 5k populated × 2 files + 5k empty directories | 656.78 / 721.98 | 533.16 / 611.94 | 522.38 / 541.38 | 0.824 | 0.799 | 0.984 |
| 20k populated × 2 files + 20k empty directories | 2726.06 / 3768.92 | 2193.96 / 2580.68 | 2244.82 / 2372.89 | 0.815 | 0.835 | 1.012 |

The standard five cases have an equal-weight geometric mean of their paired-ratio
medians of **1.015 for 8 / 4**, **0.963 for 18 / 4**, and **0.957 for 18 / 8**.
This arbitrary equal weighting illustrates the data; it is not a production
workload model. The additional large-directory case is kept separate.

The wide directory and deep chain offer little sibling-directory parallelism.
Directory-heavy cases benefit from eight workers, while eighteen shows no
consistent extra gain over eight. Small files are slower at eight and eighteen
than at four in this run.

## CPU and memory cost

Each cell is **scan CPU milliseconds / mean CPU cores / peak RSS MiB**, reporting
medians over the twelve trials. CPU milliseconds sum process user plus system
time across all threads over the scan interval. Mean cores are each trial's
CPU time divided by scan wall time. RSS is the process high-water mark sampled
when scanning returns, before diagnostic serialization, hashing and indexing;
it includes runtime/process setup. It is not final service or GUI memory.

| Fixture | 4 workers | 8 workers | 18 workers |
| --- | ---: | ---: | ---: |
| 100k small files / 100 directories | 1520.56 / 3.86 / 75.43 | 3451.04 / 7.29 / 77.22 | 6022.74 / 13.91 / 82.72 |
| One directory / 50k files | 231.61 / 1.06 / 40.71 | 230.68 / 1.06 / 40.71 | 228.67 / 1.05 / 40.71 |
| 128 levels × 64 files | 41.52 / 0.92 / 18.19 | 41.88 / 0.92 / 18.16 | 41.81 / 0.92 / 18.12 |
| 10k mixed files + hidden/link fixtures | 128.79 / 3.70 / 16.23 | 271.51 / 6.95 / 16.92 | 485.38 / 13.66 / 18.40 |
| 5k populated × 2 files + 5k empty directories | 1327.77 / 2.03 / 26.20 | 1649.87 / 3.08 / 26.43 | 1618.15 / 3.12 / 26.85 |
| 20k populated × 2 files + 20k empty directories | 5758.33 / 2.12 / 74.41 | 7209.37 / 3.20 / 76.24 | 7072.33 / 3.17 / 76.03 |

On the small-file case, eighteen workers used about **4.0× the CPU time** of
four, while the paired wall-time ratio was 1.092 and median RSS rose from
75.43 to 82.72 MiB. On the mixed case, eighteen used about **3.8× the CPU time**
for a paired wall-time ratio of 0.959, with median RSS rising from 16.23 to
18.40 MiB. Eight workers also consumed materially more CPU on these shapes.
These costs matter when scanning while the user continues using the desktop.

A conservative static Auto cap of four avoids spending those extra cores by
default. A manually selected eight workers is useful for the measured directory-
heavy cases. Full CPU-count concurrency is available for deliberate experiments,
but these results do not justify using it as the universal desktop default.
No workload-adaptive heuristic was inferred from this small synthetic suite.

## Reproduce

Python 3.11+ and the release Rust toolchain are sufficient; no Python packages
are needed. Keep other builds and scans out of the measurement window.

```sh
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release -p ohmydisk-platform --example scan-bench
python3 benchmarks/worker_matrix.py \
  --binary target/release/examples/scan-bench \
  --iterations 12 --warmups 3 --include-large-directory \
  --output benchmark-output/worker-matrix.json
```

The harness creates and removes its own fixtures, records binary/tool hashes,
actual detected/effective CPU worker counts, per-trial order and every result,
and fails on configuration, structural or inventory mismatch. The raw report
records `fixtures_cleaned` and `binary_unchanged` only after successful cleanup
and the final binary hash check. Its source commit plus dirty-diff hash identify
the measured source state; a rerun of a newer commit is a new experiment.

Tooling checks are included in normal Python test discovery and the Rust example:

```sh
python3 -m unittest discover -s tests -v
cargo test -p ohmydisk-platform --example scan-bench
```
