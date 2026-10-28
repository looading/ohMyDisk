# Scan performance

Use the core benchmark to isolate directory traversal and the service benchmark
to measure when scans and queries become usable through IPC. They serve different
purposes; a faster traversal alone does not establish faster application results.

- [Measured iterations and results](RESULTS.md)
- [Core runner, fixtures, parity checks and cancellation](SCAN_BENCH.md)
- [Service persistence, first pages and historical cold loads](SERVICE_BENCH.md)
- [Pinned v0.1.0 reference runner](reference/README.md)
- [Manual native Windows/macOS workflow](BENCHMARK_CI.md)

Both harnesses use generated temporary fixtures by default. Existing directories
are scanned only with an explicit `--root`; the harnesses never call cleanup
operations. Reports omit paths and filenames. Filesystem caches remain warm.
Correctness failures fail the run; timing thresholds are informational.
