# Core scan benchmark

`scan_bench.py` uses a separate diagnostic runner that calls the core scanner and
`NativePlatform` directly. It excludes IPC, service persistence and GUI work.
Python 3.11 or newer is sufficient; no Python packages are required.

Build the runner in release mode, then run:

```sh
cargo build --release -p ohmydisk-platform --example scan-bench
python3 benchmarks/scan_bench.py \
  --binary target/release/examples/scan-bench \
  --output docs/scan-benchmark.json
```

The default suite creates and removes its own temporary data:

| Fixture | Default contents | Purpose |
| --- | --- | --- |
| `small-files` | 100,000 files of 128 bytes, up to 1,000 per directory | Metadata-heavy traversal |
| `wide-directory` | 50,000 files of 32 bytes in one directory | Large directory enumeration and ordering |
| `deep-tree` | 128 nested directories, one file per level | Depth and parent aggregation |
| `mixed` | 10,000 varying-size files, hidden and empty directories, link target and optional hardlink/symlinks | Boundary handling and logical-size integrity |

Fixture sizes, random seed and fixture selection can be changed explicitly:

```sh
python3 benchmarks/scan_bench.py --binary target/release/examples/scan-bench \
  --fixture small-files --small-files 10000 --iterations 7 --warmups 2 \
  --cancel-after-ms 10 --cancel-iterations 3 --output docs/small-scan.json
```

`--fixture` may be repeated. Deep trees are capped at 128 levels.
`--deep-files-per-level` defaults to 1, retaining the original deep-tree fixture.
Set it to 64 to create 8,192 files across 128 levels:

```sh
python3 benchmarks/scan_bench.py --binary target/release/examples/scan-bench \
  --fixture deep-tree --deep-depth 128 --deep-files-per-level 64 \
  --output docs/deep-scan.json
```

Deep-tree file sizes are `128 + depth_index + (file_index % 1024)` bytes,
where both indices start at zero. The report records depth, files per level,
total regular files and the minimum/maximum file sizes. Link creation
depends on filesystem and OS permissions; the report records availability and
actual inventory. Relative links include a directory cycle and dangling link.
The scanner should retain each link as a boundary without traversing it.
Hardlinks count their full logical size once per directory entry, matching scan
semantics. A mixed scan with symlinks is expected to report `complete: false`
because boundary issues make the snapshot incomplete.

An explicit existing directory can be scanned read-only with `--root DIRECTORY`.
No directory is selected automatically, and the script never modifies files in
that directory. Report and temporary diagnostic output must be outside the
selected root; otherwise the script refuses the run. The root must be a real
directory. Links, reparse points, special files and directories on another
filesystem are retained as boundary nodes and are not traversed. Independent
inventories run before and after measurements. A change in path, kind, size,
inode, mtime or ctime invalidates the report. Concurrent
changes can still evade this check, so a quiescent directory is preferred.

For an old/new comparison, preserve both release executables and supply both:

```sh
python3 benchmarks/scan_bench.py \
  --binary target/release/examples/scan-bench \
  --reference-binary /path/to/preserved/scan-bench \
  --iterations 7 --output docs/scan-comparison.json
```

Both versions scan the same fixture; execution order alternates per iteration.
To rebuild the original scanner instead of relying on a preserved local binary,
use the pinned source recipe in [reference/README.md](reference/README.md).
That independent crate targets the actual v0.1.0 source revision and keeps its
own dependency lockfile.
The comparison reports candidate/reference ratios of medians. Ratios below 1
mean lower latency, snapshot size, metadata calls or RSS. Each binary is
identified by its SHA-256 and size, not its path. `--profile`,
`--reference-profile` and `--target` are declarations: this script does not build
or verify optimization settings. The default target is the local `rustc` host
when available. Use matching target and optimization settings for comparisons.

Enumeration modes can be selected independently with
`--enumeration {default,portable}` for the candidate and
`--reference-enumeration {default,portable}` for the reference. Both default to
`default`, which passes no enumeration flag and remains compatible with frozen
older runners. `portable` adds only the fixed `--portable-enumeration` flag to
that runner's full, warmup and cancellation scans. No arbitrary runner arguments
are accepted. Each binary's metadata and the report's method record the selected
mode; `default` means the runner's default implementation, which depends on its
version and platform.

To isolate native bulk enumeration from the traversal algorithm, compare the same
new executable with its portable mode:

```sh
python3 benchmarks/scan_bench.py \
  --binary target/release/examples/scan-bench \
  --reference-binary target/release/examples/scan-bench \
  --enumeration default --reference-enumeration portable \
  --output docs/scan-enumeration-comparison.json
```

To compare the new traversal algorithm against an older scanner without using
native bulk enumeration in the candidate, use `--enumeration portable` with the
new binary, and leave `--reference-enumeration default` for the preserved older
binary. Both comparisons retain the same independent inventory and digest checks.

Fixture creation, environment inspection and independent inventory times are
reported separately. Each binary gets one unmeasured warmup and five measured
scans by default. Summaries contain the median and nearest-rank p95; with five
samples p95 is the largest sample. These are warm-cache measurements: creating
and inventorying fixtures touches filesystem caches. The script does not clear
caches and makes no cold-cache claim.

`scan_ms` measures the runner's scan call. `serialize_ms` measures snapshot JSON
serialization, and `snapshot_bytes` is the serialized size. `process_wall_ms`
also includes process startup and diagnostic output. `files_per_second` divides
the snapshot's non-directory count by scan time; it includes symlink/other
boundary entries. `files` and `directories` are classified by node kind, and
`directories` includes the root and untraversed directories at filesystem
boundaries. This differs from `ScanProgress.files` at a filesystem boundary,
where the progress callback counts the untraversed directory with files.
Peak RSS is the OS process high-water mark, read immediately after the scan call
returns, before diagnostic hashing, queries or serialization. It includes process
startup and scan allocations; it is not the incremental memory used by the scan.
The value is reported in bytes when supported, otherwise null. There is no
Python subprocess sampling estimate mixed into that metric.

Cancellation runs request cancellation after 20 ms by default. They are separate
from full-scan summaries and report cancellation latency only for scans that
actually observe cancellation. A scan that finishes before the request is
recorded as `completed_before_cancel`; it does not establish cancellation
responsiveness. If reported scan time exceeds the requested delay while
`cancelled` is false, it is recorded separately as
`completed_without_observing_cancel`; this can indicate timer scheduling delay
or cancellation arriving during final aggregation. Choose a larger fixture or shorter `--cancel-after-ms` when all
attempts finish first. Set `--cancel-iterations 0` to omit these runs.

Every full scan is checked against an independent `lstat`/`scandir` inventory for
node count, non-directory count, directory count, logical bytes and completeness.
Repeated scans and both binaries must return the same stable validation digest.
The runner is responsible for deriving that digest from its node structure;
matching total bytes alone is insufficient to establish structural integrity.
Cancelled scans may contain only a prefix of nodes and are checked separately.
The runner's digest v1 hashes compact JSON containing sorted node tuples
`[relative_native_path_base64, kind, parent_relative_native_path_base64_or_null,
logical_bytes, allocated_bytes_or_null, complete, is_link_or_reparse, link_count]`,
sorted relative issue-path/code pairs, and snapshot complete/cancelled flags.
The root relative path is `.`; tuples sort by the base64 path string. UUIDs,
creation/modified times and absolute paths are excluded. This script uses its own
inventory hash rather than attempting to reproduce the Rust digest.

JSON contains aggregate fixture metadata, counts, hashes and a small allowlist of
runner scalars. It omits absolute paths, filenames, raw stdout and raw stderr.
Generated inventory digests contain no literal paths. The script exits 0 for a
validated report, 2 for a written report with validation failures, and 1 for an
execution error. Runner failures are printed with fixed labels or exit codes so
diagnostics cannot inadvertently expose a scanned path.

## Diagnostic runner contract

The script invokes an executable without a shell:

```text
ohmydisk-scan-bench <absolute-root> --output <temporary-json>
ohmydisk-scan-bench <absolute-root> --output <temporary-json> --cancel-after-ms N
```

It must return success after writing a JSON object with these fields:

```json
{
  "scan_ms": 100.0,
  "serialize_ms": 20.0,
  "snapshot_bytes": 1000000,
  "nodes": 1001,
  "files": 1000,
  "directories": 1,
  "logical_bytes": 128000,
  "complete": true,
  "cancelled": false,
  "metadata_calls": 1002,
  "metadata_ms": 20.0,
  "progress_callbacks": 10,
  "first_children_ms": 1.0,
  "first_descendants_ms": 2.0,
  "directory_calls": 100,
  "directory_ms": 10.0,
  "index_build_ms": 3.0,
  "peak_rss_bytes": 10000000,
  "validation_digest": "0000000000000000000000000000000000000000000000000000000000000000",
  "cancel_latency_ms": null
}
```

Numbers must be finite and nonnegative; counts and byte sizes must be integers.
`metadata_calls`, `metadata_ms`, `progress_callbacks`, `first_children_ms`,
`first_descendants_ms`, `directory_calls`, `directory_ms`, `index_build_ms`,
`peak_rss_bytes` and `cancel_latency_ms` may be null or omitted when unavailable.
`progress_callbacks` and `directory_calls` must be integers when provided.
Directory operation and index build metrics are included in summaries when
reported, and remain null for older runners that do not provide them.
The current diagnostic runner constructs `QueryIndex` once after scanning.
`index_build_ms` measures that construction; it is outside `scan_ms` and the
process peak-RSS reading taken when scanning returns. `first_children_ms` and
`first_descendants_ms` measure `query_indexed` against that already constructed
index. A preserved v0.1.0 diagnostic runner uses the original uncached query
implementation, so its query setup costs differ. The original `query()` remains
available as the correctness reference.
The digest must be a 64-character SHA-256 hex string, stable
across runs and independent of snapshot IDs, timestamps and absolute root paths.
Only documented fields enter the report; additional runner fields are ignored.
Cancellation latency should measure the time from setting the cancellation flag
until the scan call returns, excluding serialization. The runner should use an
interruptible timer so scans that complete before N do not wait for N to elapse.

For the manually triggered native Windows/macOS workflow and its fixed
default-versus-portable comparison, see [BENCHMARK_CI.md](BENCHMARK_CI.md).
