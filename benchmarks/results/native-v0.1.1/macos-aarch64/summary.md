# Native benchmark: macos-15

Target: `aarch64-apple-darwin`.

Timings are informational. Only execution and correctness validation affect the job result.

Core: same binary, default enumeration versus portable. This isolates the enumerator; it is not a v0.1.0 comparison.

| Validation | Passed |
| --- | --- |
| scan | True |
| service | True |

## Core scan

| Fixture | Default median ms | Portable median ms | Default / portable | Default p95 ms | Portable p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| small-files | 254.722 | 981.823 | 0.259 | 313.953 | 2921.969 |
| wide-directory | 120.675 | 240.297 | 0.502 | 159.121 | 457.360 |
| deep-tree | 9.796 | 19.009 | 0.515 | 12.812 | 35.791 |
| mixed | 18.347 | 35.361 | 0.519 | 23.178 | 82.548 |

## Service end to end

Current release build only; no historical application baseline.

| Metric | Median | p95 |
| --- | ---: | ---: |
| scan_terminal_ms | 637.444 | 649.528 |
| source_scan_ms | 241.807 | 320.657 |
| source_persist_ms | 214.085 | 239.587 |
| source_index_build_ms | 31.538 | 34.341 |
| first_list_children_ms | 9.384 | 18.575 |
| first_query_descendants_ms | 14.217 | 21.210 |
| warm_list_children_ms | 9.252 | 11.244 |
| warm_query_descendants_ms | 13.998 | 33.803 |
| sampled_max_rss_bytes | 103186432.000 | 110051328.000 |
| state_total_bytes | 9744280.000 | 9748400.000 |
| snapshot_payload_bytes | 57332283.000 | 57332283.000 |
| snapshot_stored_bytes | 4766853.000 | 4766860.000 |
| historical_cold_list_children_ms | 156.486 | 164.295 |
| historical_cold_query_descendants_ms | 170.317 | 211.451 |
| historical_warm_list_children_ms | 10.839 | 20.026 |
| historical_warm_query_descendants_ms | 22.874 | 28.845 |
| historical_children_sampled_max_rss_bytes | 157040640.000 | 180092928.000 |
| historical_descendants_sampled_max_rss_bytes | 154583040.000 | 160759808.000 |

Core query index construction is outside scan_ms and the scan-end process peak-RSS reading.
Service RPC timings include diagnostic process startup and polling; service RSS is a sample maximum, not a process peak.
Historical cold queries restart the service against saved state; OS filesystem caches remain warm.
Windows default and portable currently share the portable enumerator; those results establish parity.
One excluded warmup; nearest-rank p95; filesystem caches are not cleared.
See the JSON artifacts for all raw numeric trials, environment and binary hashes.
