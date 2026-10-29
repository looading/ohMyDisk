# Disk-space view, DaisyDisk-style GUI and concurrent scanning

Status: approved by the user on 2026-10-29: implement the complete recommended
space model, DaisyDisk-style GUI and bounded Tokio directory concurrency. The application remains
Rust + iced, with GUI and omd_ MCP tools sharing the local service.

## Product contract

The default interface presents disk-space usage against the system-reported
capacity of the selected filesystem. A roughly 995 GB filesystem must not show
an accumulated 1.1 TB logical total as its primary occupancy figure. The capacity
boundary comes from the OS, not a constant 1 TB limit, and the program never
silently truncates or proportionally rescales raw file metadata to make it fit.

Logical size remains available in details and explicit legacy/advanced queries.
Every reported metric has a name and measurement status. An unavailable physical
measurement is not zero and never falls back to logical bytes without saying so.

## Recommended space model

Use a same-basis capacity observation, captured for the scan root at scan start
and end. Let C be filesystem-visible capacity and A be raw available bytes.
Let E be the sum of known attributed file bytes in the scanned scope. If identity,
time and bounds are coherent, the root ring represents:

- raw available space A;
- known attributed file space E, with the directory hierarchy;
- shared, system and unattributed space R = C - A - E.

The ring sums to C. The main used/reserved figure is C - A. R also includes
permission gaps, files outside a folder-only scan, filesystem metadata, snapshots
and reservations. It must not be labelled exclusively as shared blocks or junk.
Physical observed used bytes, purgeable capacity and caller quota are separate
optional facts, not terms silently mixed into this equation.

When E exceeds C-A, the capacity identity changes, or observations cannot be
reconciled, show the OS capacity overview and an explicit inconsistent/partial
measurement state. Preserve raw metadata but disable the additive attribution
view. Never hide a negative remainder with max(0), scale directory values, or
claim a pie has reconciled when it has not.

The folder tree contains all discovered entries, including unknown/shared ones.
Rows distinguish known zero, partly measured and unmeasured. Shared/unattributed
space is navigable to explanatory details and known related entries, but is not
a synthetic deletion target. A folder with all bytes shared must not be described
as an empty folder merely because its measured exclusive contribution is zero.

### Native providers

macOS requests the documented ATTR_CMNEXT_PRIVATESIZE attribute in the existing
bulk enumeration with FSOPT_ATTR_CMN_EXTENDED. Check each record's returned bits;
unsupported attributes remain unknown. Do not issue a second syscall for every
file on a volume that cannot return this attribute. Keep sparse, compression,
resource-fork, native-name, timestamp and boundary semantics intact.

Windows uses native allocation information for local NTFS only when the volume
does not advertise shared-block refcounting and the entry is an ordinary,
non-reparse, single-link file. Existing compressed/sparse allocation information
is reused. ReFS, remote or unrecognized cases remain unmeasured until a provider
can establish a reliable value. All files remain browsable through explicitly
labelled logical or reported-allocation details.

For the first implementation, multi-link files are retained as shared/unattributed
rather than attributing the same bytes to several directory entries. Original
logical and reported-allocated metadata remain unchanged. This conservative rule
can be expanded to shared-group attribution later without changing raw metadata.

Capacity sampling must replace direct reliance on the current sysinfo values:
macOS uses a single native volume-attribute observation for SIZE/SPACEAVAIL, with
FREE/USED as optional detail. Windows uses GetDiskSpaceInformationW actual-volume
allocation-unit fields; caller quota and reserves are separate. Older unsupported
systems get an explicitly scoped fallback overview, not a mixture of caller and
volume counters. No admin elevation or raw device access is required.

### Data/API changes

Add a versioned optional space-observation record to Snapshot. Add optional node
space measurements with known attributed bytes, measured/unmeasured state and
provider. Defaults preserve old JSON and compressed snapshots. Old snapshots
without these fields remain readable, but the default GUI asks for a physical
rescan instead of displaying a fabricated occupancy tree.

Add size_metric to queries and apply it consistently to ordering, min/max filters,
percentages, chart weights and cursors. Retain existing explicitly named raw
logical fields and legacy API behavior. GUI requests the new metric explicitly.
Responses expose the actual metric, provider and measurement status. Do not
silently coerce unknown measurements to zero for filtering. Queries without
numeric size filters retain unknown entries. Numeric min/max filters compare
fully measured values only and return an excluded-unmeasured count; the GUI
shows that count instead of pretending those entries matched or were zero. The existing cursor
signature binds the full Query, including the new metric.

Cleanup authorization is unchanged: raw immutable plan members and independent
identity/scope validation remain authoritative. Attributed or allocated bytes
are not a promise of cleanup yield; volume observations remain the final evidence
of available-space changes.

## GUI reference and interaction

The working interpretation of dayDisk is DaisyDisk. Public official material is
sufficient; no user screenshots, license, logos or copied brand assets are needed.
If the user meant another product, its link would be the only missing reference.

Use two primary views:

- Disk overview: horizontal disk/source rows, capacity bars, explicit scan,
  view-result and rescan actions, and saved results. Related APFS entry points
  must not look like independent capacities to sum.
- Analysis: a large multicolour sunburst, slim breadcrumbs/navigation, a compact
  right-hand directory list and a collapsed bottom collector strip. Keep our own
  icons, branding and Rust/iced implementation.

Top-level branches have distinct colours; descendants inherit a related hue.
Hovering a directory previews its children in the right panel. Cached previews
appear immediately; uncached ones use a short debounce and epoch/cancellation so
old responses cannot overwrite newer hover state. Click enters the directory;
clicking the circle centre returns to its parent. A filesystem-root analysis
starts in the full capacity context. A folder-only analysis starts with its own
attributed E as the local chart budget, so a 9 MB folder is not labelled as the
whole volume's used space. At the scan root, a volume/context toggle switches
between these views only when the same occupancy ledger is valid; descendants
always use local E. This changes presentation scope, never the underlying bytes. Small-item aggregation and
unattributed space have distinct styling and interactions.

Collector defaults to a compact row and expands for review. Dragging highlights
the collector and shows the item's name. Existing parent/child selection
normalization, plan preparation and separate trash/permanent confirmation remain.

Scanning uses compact status, location, item counts, elapsed time and stop control.
There is no percentage without a known denominator. Refreshing a saved result
keeps the immutable old result visible with its capture/refresh status. Primary
numbers use the occupancy model; raw logical sums appear only in details or an
explicit metric switch.

## Concurrent scanning architecture

Tokio already exists and runs jobs concurrently. This change adds parallelism
inside one scan instead of adding a second runtime or spawning a task per file.

Use one coordinator/reducer that owns nodes, parent indices, seen directory
identities, issues and progress. Fixed-capacity directory workers execute blocking
filesystem operations on Tokio's blocking pool. Native macOS bulk regular-file
metadata is reused. Batches are bounded by entries and bytes and carried through
a bounded channel. Initial benchmark candidates are 1/2/4/8 workers; the measured
result selects the default, with a process-wide filesystem budget across scans.

Workers obtain fresh directory metadata immediately before admission. The reducer
checks volume, link/reparse status and directory identity, creates the node and
approves reading. Only approved work enters a directory. Subdirectory candidates
become eligible after their parent enumeration completes, retaining the existing
queued-boundary regression semantics. Portable enumeration may add a nontrusted
kind hint to avoid an extra stat for every directory; all traversal decisions
still require fresh metadata.

A parent node is created before its children, so reverse aggregation remains
valid even when sibling batches arrive out of order. IDs remain opaque and stable
inside one immutable snapshot; cross-scan arrival order is not an ID contract.

Cancellation stops new dispatch/admission, wakes workers blocked on queues or
replies, marks partial state and joins every in-flight worker before publishing
terminal status. A blocking OS call cannot be stopped by aborting a Tokio handle;
no worker may keep growing a returned cancelled snapshot. Panic/error paths use
the same coordinated shutdown. Progress is throttled centrally and terminal state
is always committed. Aggregation, encoding and index construction stay off the
async executor's latency-sensitive tasks.

Active workers and transport buffers are bounded. The Snapshot and directory
frontier still grow with the number of discovered entries; no constant-memory
claim is made. A spill queue or resumable directory cursor would be a separate
large-tree memory extension if benchmark evidence requires it.

Queries share immutable views, have a separate limit for expensive filters and
do not consume scan-worker permits. Job status and cancellation remain responsive.
Move cold-cache construction toward per-snapshot single-flight so loading one
snapshot does not block ready-cache hits for a different snapshot. Keep SQLite
encoding/decoding outside its connection lock and retain cleanup FULL durability.

## Acceptance and measurement

Correctness cases include: logical size larger than capacity with coherent bounded
space overview; sparse files; APFS shared/private data; hardlinks; unsupported
providers; partial permissions; legacy snapshots; quota/reservation scope; and
capacity changes during a scan. Example C=995 GB, A=245 GB, E=500 GB yields
R=250 GB and a 995 GB root, regardless of an advanced logical total of 1.1 TB.

Concurrency tests cover simultaneous alias admission, ancestor cycles, queued
link/volume changes, partial directory errors, cancellation with full queues,
worker panic, worker joining and no post-cancellation mutations. Existing semantic
digests and cleanup-boundary tests remain required.

Performance uses release binaries, one warmup, at least five measured runs and
alternating order. Compare workers 1/2/4/8 on small files, a wide directory, deep
chains, many directories, empty-directory-heavy and mixed fixtures. Include one
scan, two scans, scan plus warm query and scan plus cold query. Record scan/stage
latency, median/p95, RSS, queue/batch high water, metadata calls and cancellation.
Time fixture creation separately and make no cold-cache claim without measuring it.
Concurrent tasks must earn their default setting through these measurements.

Native Windows x64/macOS Intel/macOS ARM CI runs correctness and MCP smoke. GUI
interaction validation uses an isolated temporary state/fixture, never restarts
or cancels the user's current application or scans their home without selecting it.
Only tested CI-built distribution assets are published, with checksums and privacy
marker checks. No messages are sent to other services or people.

## Official references

- DaisyDisk interface: https://daisydiskapp.com/
- Sunburst interaction: https://daisydiskapp.com/guide/4/en/UnderstandingSunburst/
- Shared/hidden space: https://daisydiskapp.com/guide/4/en/HiddenSpace/
- Hard links and clones: https://daisydiskapp.com/guide/hard-links
- Apple documented attributes: https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/man/man2/getattrlist.2
- Apple bulk attribute support: https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/man/man2/getattrlistbulk.2
- Windows volume/caller counters: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/ns-fileapi-disk_space_information
- Windows allocation: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_standard_info
- Windows shared-block capability: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationbyhandlew
