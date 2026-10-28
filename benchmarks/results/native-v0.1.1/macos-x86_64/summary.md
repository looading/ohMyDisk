# Native benchmark: macos-15-intel

Target: `x86_64-apple-darwin`.

Timings are informational. Only execution and correctness validation affect the job result.

Core: same binary, default enumeration versus portable. This isolates the enumerator; it is not a v0.1.0 comparison.

| Validation | Passed |
| --- | --- |
| scan | True |
| service | True |

## Core scan

| Fixture | Default median ms | Portable median ms | Default / portable | Default p95 ms | Portable p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| small-files | 317.681 | 618.018 | 0.514 | 364.378 | 726.504 |
| wide-directory | 160.077 | 339.241 | 0.472 | 161.438 | 347.909 |
| deep-tree | 15.569 | 40.395 | 0.385 | 16.530 | 43.858 |
| mixed | 35.826 | 76.474 | 0.468 | 111.619 | 162.818 |

## Service end to end

Current release build only; no historical application baseline.

| Metric | Median | p95 |
| --- | ---: | ---: |
| scan_terminal_ms | 947.630 | 1149.881 |
| source_scan_ms | 325.157 | 495.137 |
| source_persist_ms | 468.476 | 537.339 |
| source_index_build_ms | 31.016 | 35.333 |
| first_list_children_ms | 16.201 | 18.856 |
| first_query_descendants_ms | 26.739 | 27.766 |
| warm_list_children_ms | 16.932 | 18.860 |
| warm_query_descendants_ms | 26.675 | 28.743 |
| sampled_max_rss_bytes | 101855232.000 | 108929024.000 |
| state_total_bytes | 10118120.000 | 10118120.000 |
| snapshot_payload_bytes | 57732687.000 | 57732687.000 |
| snapshot_stored_bytes | 4947403.000 | 4947404.000 |
| historical_cold_list_children_ms | 270.346 | 322.338 |
| historical_cold_query_descendants_ms | 277.818 | 283.823 |
| historical_warm_list_children_ms | 17.119 | 23.970 |
| historical_warm_query_descendants_ms | 26.609 | 29.098 |
| historical_children_sampled_max_rss_bytes | 141930496.000 | 142045184.000 |
| historical_descendants_sampled_max_rss_bytes | 144760832.000 | 145051648.000 |

Core query index construction is outside scan_ms and the scan-end process peak-RSS reading.
Service RPC timings include diagnostic process startup and polling; service RSS is a sample maximum, not a process peak.
Historical cold queries restart the service against saved state; OS filesystem caches remain warm.
Windows default and portable currently share the portable enumerator; those results establish parity.
One excluded warmup; nearest-rank p95; filesystem caches are not cleared.
See the JSON artifacts for all raw numeric trials, environment and binary hashes.
