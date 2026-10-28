# Native benchmark: windows-2022

Target: `x86_64-pc-windows-msvc`.

Timings are informational. Only execution and correctness validation affect the job result.

Core: same binary, default enumeration versus portable. This isolates the enumerator; it is not a v0.1.0 comparison.

| Validation | Passed |
| --- | --- |
| scan | True |
| service | True |

## Core scan

| Fixture | Default median ms | Portable median ms | Default / portable | Default p95 ms | Portable p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| small-files | 2612.437 | 2622.257 | 0.996 | 2689.484 | 2647.842 |
| wide-directory | 1296.020 | 1290.724 | 1.004 | 1300.500 | 1293.674 |
| deep-tree | 120.052 | 119.960 | 1.001 | 120.757 | 120.311 |
| mixed | 270.921 | 268.473 | 1.009 | 272.390 | 268.828 |

## Service end to end

Current release build only; no historical application baseline.

| Metric | Median | p95 |
| --- | ---: | ---: |
| scan_terminal_ms | 3274.297 | 3306.761 |
| source_scan_ms | 2876.210 | 2909.512 |
| source_persist_ms | 313.529 | 317.758 |
| source_index_build_ms | 11.616 | 12.148 |
| first_list_children_ms | 11.668 | 13.101 |
| first_query_descendants_ms | 17.173 | 18.489 |
| warm_list_children_ms | 12.136 | 13.164 |
| warm_query_descendants_ms | 17.023 | 18.262 |
| sampled_max_rss_bytes | n/a | n/a |
| state_total_bytes | 4123936.000 | 4123936.000 |
| snapshot_payload_bytes | 73547114.000 | 73547114.000 |
| snapshot_stored_bytes | 3899568.000 | 3899568.000 |
| historical_cold_list_children_ms | 220.542 | 221.815 |
| historical_cold_query_descendants_ms | 224.216 | 229.306 |
| historical_warm_list_children_ms | 12.700 | 12.874 |
| historical_warm_query_descendants_ms | 18.626 | 28.041 |
| historical_children_sampled_max_rss_bytes | n/a | n/a |
| historical_descendants_sampled_max_rss_bytes | n/a | n/a |

Core query index construction is outside scan_ms and the scan-end process peak-RSS reading.
Service RPC timings include diagnostic process startup and polling; service RSS is a sample maximum, not a process peak.
Historical cold queries restart the service against saved state; OS filesystem caches remain warm.
Windows default and portable currently share the portable enumerator; those results establish parity.
One excluded warmup; nearest-rank p95; filesystem caches are not cleared.
See the JSON artifacts for all raw numeric trials, environment and binary hashes.
