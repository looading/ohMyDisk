# Occupancy and directory concurrency experiments

The default occupied-space view and the scanner now collect more information
than v0.1.2's logical-size traversal. Compare equivalent measurement modes when
judging concurrency. This report retains both improvements and regressions.

Local hardware: Apple M5 Pro, 18 cores, 48 GiB, macOS 26.6.2 (25G83), APFS.
All measured executable variants use the release profile and rustc 1.94.1 for
`aarch64-apple-darwin`. Each role in every pair has one excluded warmup, five
measured samples in alternating role order, and three separate cancellation
probes. p95 is nearest rank, therefore the
largest of five samples. Fixture setup/inventory/cleanup is outside scan time.
Filesystem caches remain warm; no cold-disk or real-home timing claim is made.

Only owned generated fixtures were used. No builds, tests or other benchmark ran
alongside the timed windows. Reports omit scanned filenames and private paths,
preserve binary hashes/configuration and validate independent inventory and canonical
snapshot digest v1 before accepting timings. Digest v1 projects the original
logical/allocated/path graph; native attribution has separate sparse/clone and
missing-value tests and occupancy reconciliation checks.

## First concurrency matrix

Five shapes were measured using fixed 1/2/4/8 async workers, each paired against
the current serial scanner. Another pair compares current serial with frozen
v0.1.2. These 25 groups contain 300 full/warmup and 150 cancellation scans. Every
shape has one structural digest across all 60 full/warmup runs and matching
independent inventories.

| Shape | Async 1 median / p95 ms | Async 2 | Async 4 | Async 8 |
| --- | ---: | ---: | ---: | ---: |
| 100k small files, 100 directories | 352.64 / 383.42 | 383.98 / 417.75 | 405.43 / 419.33 | 475.95 / 498.17 |
| One wide directory, 50k files | 216.34 / 270.30 | 200.18 / 217.96 | 296.28 / 315.46 | 200.80 / 222.43 |
| 128 levels, 64 files per level | 48.89 / 57.51 | 54.90 / 63.88 | 49.94 / 56.06 | 48.08 / 49.12 |
| Mixed 10k | 44.39 / 45.20 | 42.40 / 44.59 | 35.33 / 37.42 | 40.84 / 44.41 |
| 5k directories × 2 files, plus 5k empty directories | 1681.25 / 1746.02 | 899.44 / 1004.10 | 635.84 / 683.99 | 520.86 / 543.75 |

There is no universal best worker count. Directory-heavy work benefits from
overlapping directory I/O, while hot file-heavy directories can slow down through
contention and scheduling. A wide directory and a deep chain do not provide many
sibling directories to parallelize. Eight workers are therefore not selected
solely because an arbitrarily equal-weight mean favors them.

The current serial implementation also differs from the old logical-only one:

| Shape | Current serial median ms | v0.1.2 median ms |
| --- | ---: | ---: |
| Small 100k | 357.98 | 151.60 |
| Wide 50k | 237.51 | 72.14 |
| Deep 128 × 64 | 43.98 | 27.53 |
| Mixed 10k | 51.44 | 24.59 |
| Directory-heavy | 1284.82 | 1215.05 |

These are different measurement workloads. The new private-size query and
per-node measurement bookkeeping have costs. The measured before/after capacity
observations total only about 0.02–0.03 ms and metadata-method counts are unchanged;
the extra time is inside directory enumeration and its visitor. That evidence
does not isolate the exact kernel versus user-space contribution. No field or
measurement is skipped to claim a speedup. In particular, clone flags do not
prove that blocks are unpinned by snapshots, so they are not substituted for
private size merely to make ordinary-file benchmarks faster.

Raw reports, hashes and all trials:
[round1 matrix summary](results/2026-10-29/occupied-concurrency/round1/matrix-summary.json),
[metadata and report index](results/2026-10-29/occupied-concurrency/round1/matrix-metadata.json).

## Second scheduling iteration

Each directory now uses one blocking closure for fresh metadata, admission and
enumeration instead of two. The same filesystem permit covers these stages;
the reducer never waits for that permit. Batches allocate lazily and reserve a
fixed capacity after transfer, avoiding repeated growth and empty-directory
allocations. Attribution semantics are unchanged.

Twelve representative pairs retain one warmup, five samples and three cancel
probes per role. All 144 full/warmup and 72 cancellation scans passed; each shape
has one structural digest across its 48 full/warmup runs.

Same-worker before/after results show where this change helps:

| Shape | Workers | Round2 median ms | Round1 paired median ms | Change |
| --- | ---: | ---: | ---: | ---: |
| Small 100k | 1 | 317.74 | 320.06 | -0.7% |
| Small 100k | 4 | 402.04 | 401.18 | +0.2% |
| Small 100k | 8 | 458.46 | 459.33 | -0.2% |
| Directory-heavy | 1 | 1618.09 | 1817.46 | -11.0% |
| Directory-heavy | 4 | 555.76 | 587.33 | -5.4% |
| Directory-heavy | 8 | 483.72 | 502.71 | -3.8% |
| Mixed 10k | 1 | 44.06 | 44.78 | -1.6% |
| Mixed 10k | 4 | 38.21 | 38.76 | -1.4% |
| Mixed 10k | 8 | 43.52 | 46.29 | -6.0% |

Small-file differences are effectively unchanged and must not be presented as
an improvement. The per-directory reduction is clearer on directory-heavy work.

The service retains four workers by default, with a process-wide eight-permit
filesystem budget shared by concurrent scans. Each core runner trial below runs
one scan and gives it a filesystem budget equal to its selected worker count;
these trials do not measure two simultaneous service scans. The default's
separate paired comparison against the same current serial measurement is:

| Shape | Async 4 median / p95 ms | Serial median / p95 ms | Median result |
| --- | ---: | ---: | ---: |
| Small 100k | 352.68 / 356.87 | 312.08 / 327.62 | 13.0% slower |
| Directory-heavy | 548.97 / 571.63 | 1259.32 / 1291.75 | 56.4% faster |
| Mixed 10k | 39.41 / 40.03 | 43.64 / 45.07 | 9.7% faster |

Use these paired ratios rather than mixing absolute times from different groups.
MCP callers may set `workers` from 1 to 8; a smaller setting is useful when
metadata is already hot or a device does not benefit. The scanner does not create
a task for every file. Snapshot and pending-directory memory remain O(entries),
while active workers and transport batches are bounded.

RSS stayed similar in same-N round2 pairs: small/N4 77.27 versus 77.30 MiB,
directory-heavy/N4 26.52 versus 26.73 MiB, mixed/N4 16.22 versus 16.31 MiB.
These are process high-water marks at scan return, including runtime startup,
before serialization/index/query work. Default N4 cancellation p95 in the three
serial comparisons was 3.14, 1.28 and 1.76 ms, with all three cancellation probes
observed in each comparison.
An in-progress OS call cannot be forcibly interrupted; cancellation waits for
that call and all workers to exit.

[Round2 summary, p95/RSS/cancellation and report index](results/2026-10-29/occupied-concurrency/round2/round2-summary.json)
contains the full evidence.

## End-to-end service and historical queries

The final 0.2.0 release executable was paired against frozen v0.1.2 on 100,000
generated small files in 100 child directories. Each role had one excluded
warmup and five alternating measured trials, with fresh state and two historical
service restarts per trial. Independent before/after inventory validation passed.
The candidate scans with the new physical measurements and queries `attributed`;
the reference scans with the legacy measurements and queries `logical`. This is
an end-to-end cost comparison of different measurement workloads, **not evidence
of an equivalent-work scan speedup**.

| Stage | 0.2.0 attributed median / p95 ms | v0.1.2 logical median / p95 ms |
| --- | ---: | ---: |
| Source scan | 347.18 / 350.06 | 125.77 / 129.39 |
| Snapshot persistence | 148.64 / 170.68 | 149.58 / 165.94 |
| Index construction | 10.14 / 11.21 | 8.23 / 8.66 |
| Terminal observed by polling client | 582.00 / 591.22 | 348.84 / 370.24 |
| First children page | 6.89 / 8.72 | 7.25 / 8.23 |
| First descendants page | 9.39 / 10.20 | 8.87 / 9.14 |
| Historical cold children | 84.89 / 86.85 | 76.61 / 81.54 |
| Historical cold descendants | 86.37 / 88.28 | 77.06 / 78.69 |
| Historical warm children | 7.69 / 8.77 | 7.75 / 9.46 |
| Historical warm descendants | 9.50 / 10.26 | 9.08 / 9.58 |

The new source scan is substantially slower than v0.1.2 on this hot small-file
fixture because it performs the additional native attribution and bookkeeping
work discussed above. Persistence remains similar, and cached query times are
similar at this scale. Cold historical queries rebuild the additional metric
ranking. Client query timings include launching a diagnostic CLI, JSON/RPC work
and response parsing; they do not measure an in-process query in isolation.
The terminal measurement also includes polling delays and CLI process overhead.

Median sampled service RSS was 106.53 MiB versus 98.05 MiB; these are `ps` samples,
not process peak RSS. Original JSON payload was 65,931,348 versus 57,532,485 bytes;
compressed payload medians were 5,089,535 versus 4,922,378 bytes. The candidate
used four scan workers and wrote three coalesced progress records versus two in
the reference. No GUI rendering or polling was measured. The separate functional
cache tests verify that a slow cold build does not block a ready hit for another
snapshot, and that same-ID loads coalesce; the sequential benchmark is not a
measurement of concurrent-client throughput.

[All service trials, binary hashes and configuration](results/2026-10-29/occupied-concurrency/service/100k-attributed-vs-legacy-logical.json).

## Reproduction

Build the current runner with:

```sh
cargo build --locked --release -p ohmydisk-platform --example scan-bench
```

The runner accepts `--workers` values `1`, `2`, `4` or `8`, or the mutually
exclusive `--serial`. The Python harness selects candidate and reference worker
modes independently. Recorded fixture generation used seed **20261029** and the
following parameters; the cancellation delay is part of each report's metadata.

| Shape | Harness fixture and size arguments | Cancellation delay |
| --- | --- | ---: |
| Small 100k | `--fixture small-files --small-files 100000` | 10 ms |
| Wide 50k | `--fixture wide-directory --wide-files 50000` | 10 ms |
| Deep 128 × 64 | `--fixture deep-tree --deep-depth 128 --deep-files-per-level 64` | 5 ms |
| Mixed 10k | `--fixture mixed --mixed-files 10000` | 5 ms |
| Directory-heavy | `--fixture directory-heavy --directory-count 5000 --directory-files-per-dir 2 --empty-directories 5000` | 5 ms |

For example, repeat the final four-worker/serial directory-heavy comparison with:

```sh
python3 benchmarks/scan_bench.py \
  --binary target/release/examples/scan-bench \
  --reference-binary target/release/examples/scan-bench \
  --workers 4 --reference-serial \
  --fixture directory-heavy --directory-count 5000 \
  --directory-files-per-dir 2 --empty-directories 5000 \
  --seed 20261029 --iterations 5 --warmups 1 \
  --cancel-iterations 3 --cancel-after-ms 5 \
  --output benchmark-output/directory-heavy-workers4-vs-serial.json
```

The recorded matrices created each owned fixture once, then reused it through
`--root` for all modes. Scratch and report output stayed outside that root. The
example above regenerates an equivalent logical fixture and keeps its setup
outside timings. Repeating physical allocation or cache history byte-for-byte is
not promised. Directory-heavy remains explicitly selected; `--fixture all`
continues to select the original four shapes.

A current build repeats the final serial/async comparison, not the earlier
scheduler. Repeating the first-round/second-round comparison requires the two
preserved release executables identified by the report hashes: use the second
round as `--binary`, the first round as `--reference-binary`, and pair
`--workers N --reference-workers N` for `N` equal to 1, 4 or 8. For old/new serial,
select the preserved v0.1.2 executable as the reference, pass `--serial` only to
the candidate, and leave reference worker/serial options unset. Frozen older
runners must not receive the new execution flags.

See [SCAN_BENCH.md](SCAN_BENCH.md) for the runner contract, exact flags,
inventory/digest rules and metrics. Current raw results expose actual scan mode
and worker count; metadata records the selected mode even when a legacy runner
has no corresponding output fields.

Service measurements can select `--size-metric attributed` to exercise the GUI's
query metric. An independently selected reference metric is recorded, and a
nonlogical request must be echoed by the server. Different metrics are different
workloads. The service harness additionally measures persistence/index time and
historical cold/warm query latency; see [SERVICE_BENCH.md](SERVICE_BENCH.md).

The GUI capacity equation is a separate correctness contract. It does not prove
that per-file attribution is correct by itself. Native returned-bit validation,
unknown-value handling, hardlink policy, filesystem capability checks, and
sparse-file and clone tests provide that evidence. Default UI never
proportionally scales file bytes or clips logical metadata to capacity to obtain
a plausible-looking graph.
