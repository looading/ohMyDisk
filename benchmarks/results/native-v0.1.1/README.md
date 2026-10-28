# Native v0.1.1 benchmark evidence

[Workflow run 40717898553](https://github.com/looading/ohMyDisk/actions/runs/40717898553)
passed all three native jobs. Reports were downloaded and verified: all four core
fixtures have matching inventories/digests; every service trial preserves full
query responses after independent restarts. Application Rust sources, manifests
and Cargo config match the v0.1.1 tag. Metadata records the exact measured commit.

This is the standard profile, Rust 1.94.1 release/thin LTO, one excluded warmup
and five alternating measured runs. Fixtures contain 100,000 small files, a
50,000-file wide directory, a 64-level tree with 64 files per level, and 10,000
mixed files plus boundary fixtures. Filesystem caches remain warm. p95 is the
largest of five samples. Each target runs on a different hosted machine; these
numbers are not a ranking of operating systems or architectures.

The core comparison uses the same current binary in default and portable modes.
It isolates native enumeration on macOS. Windows uses the portable implementation
in both modes as a control. It is not a v0.1.0 comparison. Service timings are
current-only and include diagnostic client startup/polling. Historical cold means
a new service memory cache, not a cold OS filesystem cache.

## Core scan, median / p95 milliseconds

| Target | Fixture | Default | Portable |
| --- | --- | ---: | ---: |
| windows-x86_64 | small-files | 2612.44 / 2689.48 | 2622.26 / 2647.84 |
| windows-x86_64 | wide-directory | 1296.02 / 1300.50 | 1290.72 / 1293.67 |
| windows-x86_64 | deep-tree | 120.05 / 120.76 | 119.96 / 120.31 |
| windows-x86_64 | mixed | 270.92 / 272.39 | 268.47 / 268.83 |
| macos-x86_64 | small-files | 317.68 / 364.38 | 618.02 / 726.50 |
| macos-x86_64 | wide-directory | 160.08 / 161.44 | 339.24 / 347.91 |
| macos-x86_64 | deep-tree | 15.57 / 16.53 | 40.40 / 43.86 |
| macos-x86_64 | mixed | 35.83 / 111.62 | 76.47 / 162.82 |
| macos-aarch64 | small-files | 254.72 / 313.95 | 981.82 / 2921.97 |
| macos-aarch64 | wide-directory | 120.68 / 159.12 | 240.30 / 457.36 |
| macos-aarch64 | deep-tree | 9.80 / 12.81 | 19.01 / 35.79 |
| macos-aarch64 | mixed | 18.35 / 23.18 | 35.36 / 82.55 |

## Service medians, milliseconds

| Target | Scan through terminal | Fresh children | Fresh descendants | Historical cold children | Historical warm children |
| --- | ---: | ---: | ---: | ---: | ---: |
| windows-x86_64 | 3274.30 | 11.67 | 17.17 | 220.54 | 12.70 |
| macos-x86_64 | 947.63 | 16.20 | 26.74 | 270.35 | 17.12 |
| macos-aarch64 | 637.44 | 9.38 | 14.22 | 156.49 | 10.84 |

Each target directory retains `scan.json`, `service.json`, `metadata.json`,
`status.json` and the original Actions `summary.md`. These are aggregate reports;
no user paths, file names, snapshots, SQLite files or raw logs are included.
Windows RSS is unavailable and is recorded as null. Core RSS is a process
high-water mark at scan return; service RSS on macOS is sampled, not a true peak.

For the local old/new comparison, interpretation and remaining tradeoffs, see
[RESULTS.md](../../RESULTS.md). These native samples supplement that comparison
and validate the repeatable cross-platform benchmark flow.
