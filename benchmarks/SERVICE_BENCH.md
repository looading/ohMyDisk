# Service scan benchmark

`service_bench.py` measures the application's service path, including durable
progress and snapshot storage. It complements the core scanner runner in
`SCAN_BENCH.md`. Python 3.11 or newer is sufficient; no packages are required.
Build and preserve application binaries in release mode before measuring. The
script does not build them or verify their optimization settings.

For an isolated decoder experiment, `decode-bench` constructs a synthetic
100,000-node Snapshot in memory and uses the production streaming encoder. It
compares `from_slice` with validating UTF-8 once and calling `from_str`, using
two warmups and seven alternating pairs. Every decoded snapshot must retain the
same full-field SHA-256 fingerprint. It never reads user directories or calls
cleanup, and rejects a debug build:

```sh
cargo run --locked --release -p ohmydisk-service --example decode-bench \
  > decode-comparison.json
```

Its timings include frame expansion, validation, parsing and temporary JSON
release. Fixture creation, encoding, fingerprinting and Snapshot release are
excluded. It reports no RSS, and its decode improvement is not an end-to-end
application claim. Raw JSON bytes temporarily coexist with the decoded snapshot.

```sh
cargo build --release -p ohmydisk
python3 benchmarks/service_bench.py \
  --binary target/release/ohmydisk \
  --output docs/service-benchmark.json
```

The default fixture has 100,000 ordinary files containing 128 bytes each, with
at most 1,000 files per directory. The script checks the scanner's file count,
directory count, logical bytes and completeness against these known values.
`--files N` changes its size. Fixture creation and cleanup are outside timings.

There is one excluded warmup and five measured trials per binary by default.
Each trial starts the selected binary as `service --stay-alive`, with a fresh
private state directory outside the scanned root. The runner waits for the
service ownership lock and SQLite file before probing `get_capabilities` for
RPC readiness. It also holds the application's startup lock so a diagnostic
client cannot auto-spawn an unowned replacement if the foreground service dies.
On Unix the runner creates that startup lock with mode `0600`, matching the
application's private coordination-file requirements.
Only the process started by that trial is terminated and reaped; state is
removed after its handles close.

Use `--historical-restart` to also measure opening a persisted scan with an empty
service memory cache. Each trial, including the excluded warmup, then has two
additional owned service lifetimes using the same state directory and completed
scan. The first restarted service receives `list_children` as its first snapshot
request; the second independently restarted service receives `query_nodes` with
descendants as its first snapshot request. Each cold request is followed by an
identical warm first-page request. Separately restarting for each operation keeps
the children query from warming the descendants measurement.

```sh
python3 benchmarks/service_bench.py \
  --binary target/release/ohmydisk \
  --reference-binary /path/to/preserved/ohmydisk \
  --historical-restart \
  --output docs/service-historical-comparison.json
```

Restart readiness only probes ownership and `get_capabilities`, which does not
request a snapshot. After the timed queries, the runner reads the persisted scan
summary. Complete page dictionaries, including node values, order, cursors and
metadata, and the full scan summary must equal their original fresh-scan values;
otherwise the trial fails. No rescan is started. Only scalar timings, sample
counts and `historical_results_verified: true` enter the report; pages, IDs and
paths are not retained. The startup lock stays held across all three service
lifetimes, and each process is terminated and reaped before the next starts.
The option works with the original service API and does not require a new
diagnostic endpoint. Without it, historical measurements remain null and the
original trial flow is unchanged.

To compare two preserved release binaries:

```sh
python3 benchmarks/service_bench.py \
  --binary target/release/ohmydisk \
  --reference-binary /path/to/preserved/ohmydisk \
  --output docs/service-comparison.json
```

Both binaries scan the same root. Order alternates on every round, including
the excluded warmup. The report identifies each binary by SHA-256 and reports
candidate/reference ratios of medians. Values below 1 mean a lower observed
latency, RSS sample or byte count. `--runs` can change the measured count; use at
least five for comparisons. `--files 25 --runs 1` is sufficient for a smoke run.

An existing directory is used only when explicitly selected:

```sh
python3 benchmarks/service_bench.py \
  --binary target/release/ohmydisk \
  --root /absolute/selected/directory \
  --output /outside/selected/directory/service-report.json
```

The existing root is read only. The script never calls application cleanup
operations and only removes its own generated temporary workspace. An
independent `lstat`/`scandir` inventory runs before and after all trials, using
the same implementation as the core benchmark. A difference in ordinary
directory entries, metadata, size, inode, mtime or ctime invalidates the run;
concurrent changes can evade this check, so select a quiescent root. Inventory
checks do not hash file contents. On error, no successful report is written.

`--scratch-dir` selects an existing parent for the temporary workspace. Both
scratch and output must be outside the selected root. A root containing every
available scratch location is rejected before inventory. In particular, `/`
cannot satisfy an outside-root state-directory constraint; benchmark a user
directory or a mounted volume with scratch storage outside it instead.

The JSON report contains these measurements, in milliseconds unless named
otherwise:

| Field | Meaning |
| --- | --- |
| `start_scan_accept_ms` | Wall time for diagnostic `call start_scan` to return |
| `scan_terminal_ms` | From before `start_scan` until a diagnostic poll returns a terminal job |
| `poll_rpc_total_ms`, `poll_count` | Cumulative diagnostic polling wall time and number of polls |
| `get_scan_ms` | Summary lookup after terminal completion |
| `first_list_children_ms` | First root children query, limit 100 |
| `first_query_descendants_ms` | First root descendants query, limit 500 |
| `warm_list_children_ms`, `warm_query_descendants_ms` | Second identical queries in the same service |
| `sampled_max_rss_bytes`, `rss_samples` | Maximum observed service RSS from `ps`, and sample count |
| `state_database_bytes`, `state_wal_bytes`, `state_shm_bytes`, `state_total_bytes` | File sizes while the service remains alive, without forcing a checkpoint |
| `snapshot_payload_bytes` | Original snapshot JSON bytes: legacy UTF-8 JSON length plus `snapshot_blobs.json_bytes` |
| `snapshot_stored_bytes` | Stored payload bytes: legacy UTF-8 JSON length plus compressed BLOB length |
| `snapshot_compression_ratio` | Stored/original payload bytes; null when original payload bytes are zero |
| `source_scan_ms`, `source_persist_ms`, `source_index_build_ms` | Optional scan, persistence and index build timings from terminal `job.result` |
| `progress_writes` | Optional scan callback progress writes from `job.result`; excludes indexing, terminal and cleanup records |
| `historical_cold_list_children_ms`, `historical_cold_query_descendants_ms` | First snapshot query after separate service restarts, with the same state and scan |
| `historical_warm_list_children_ms`, `historical_warm_query_descendants_ms` | Identical first-page query repeated after its historical cold request |
| `historical_children_restart_ms`, `historical_descendants_restart_ms` | Process launch until RPC readiness for each historical phase, separate from query latency |
| `historical_children_sampled_max_rss_bytes`, `historical_descendants_sampled_max_rss_bytes` | Maximum observed RSS during each historical phase, sampled separately from the fresh scan |
| `historical_children_rss_samples`, `historical_descendants_rss_samples` | RSS sample count in each historical phase |

Queries run sequentially: first children, first descendants, warm children,
warm descendants. This isolates each operation's wall time; the GUI normally
launches the first two together. The script verifies that repeated query results
are identical. Snapshot sizes are read through a read-only SQLite connection
outside the scan/query timings. The runner first checks table existence, then
sums legacy `entities` rows with `kind='snapshot'` and new
`snapshot_blobs(id, codec, payload BLOB, json_bytes)` rows. Either table may be
absent, and both may coexist; no database migration or mutation is performed.
Legacy snapshots contribute the same UTF-8 byte count to original and stored
sizes. Compressed snapshots contribute `json_bytes` to the original size and
`length(payload)` to the stored size. These are payload sizes, distinct from the
SQLite/WAL/SHM file sizes that also include page and index overhead.

The optional source measurements are copied only from numeric
`job.result.scan_ms`, `persist_ms`, `index_build_ms` and `progress_writes` values.
They remain null when an older binary omits them; summaries and comparison ratios
also remain null when no samples are available. External RPC and terminal timing
definitions are unchanged by these additional measurements.

All RPC measurements include spawning the diagnostic CLI, its IPC work and
parsing its JSON result. `scan_terminal_ms` also includes polling overhead and
the configured sleep after nonterminal polls (`--poll-interval`, default 50ms).
It is an externally observed completion time, not scanner-only CPU or I/O time.
Service startup, fixture generation, inventory and teardown are excluded.
The GUI's 650ms polling interval, chart layout and drawing are not measured.
Historical cold timings include the service's persisted snapshot read, decode
and any index construction needed by that query, plus the usual diagnostic CLI
and IPC overhead. They do not include service restart readiness. Historical
phases begin after all original fresh-scan measurements and state-size reads,
so the original metrics retain their scope.

RSS sampling runs concurrently (`--rss-interval`, default 100ms). Sampling adds
some process overhead and can miss short-lived allocations. It is an observed
sample maximum, never a true process peak. It is null when `ps` RSS is
unavailable, including Windows. Raw sample output is not retained.
With `--historical-restart`, each restarted service gets its own sampler, from
RPC readiness through the cold and warm queries and summary validation. These
samples do not share a maximum with the original scan process. Sampling still
can miss short-lived decode allocations and is not a high-water mark.

Results include median and nearest-rank p95; with five trials, p95 is the largest
sample. Creation and inventory touch filesystem caches, and subsequent trials
reuse the same root. These are warm-cache measurements. The script neither
clears OS caches nor claims cold-cache performance.
In historical metrics, “cold” refers only to the newly started service's memory
cache. Persisted data and filesystem metadata may remain in the OS cache after
the scan and previous queries. Both historical operations reuse the same on-disk
snapshot and OS cache state; their service memory caches are independently empty.

Reports retain fixed labels, numeric values, binary/inventory hashes and local
OS/architecture/Python versions. They omit selected paths, names, raw stdout,
raw stderr and exception messages. Failures use a fixed JSON error code and exit
status 2; success exits 0. Diagnostic subprocess output is parsed transiently
and never copied into a report or error.
