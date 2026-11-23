# Comparator Studio

Compare folders, review file changes, and copy them between locations on Linux, macOS, and Windows. The app also compares individual files and supports English and Russian.

# Comparing multiple terabytes

The new **Quick check** (default) avoids the very expensive two-full-drive hash pass:

1. Walk both file trees once, gathering **paths, lengths, and modification times** without reading file data. No large content is loaded into RAM.
2. If two files have the **same relative path, length and modification time**, Quick check treats them as **likely unchanged** and skips reading their contents. This is an optimization, **not proof of identical content**. The result screen discloses the number of metadata-only matches.
3. If same-path files have the **same length but different dates**, compute BLAKE3 hashes for both to determine whether the bytes differ.
4. For files with **different names or locations**, group by file length. Only file-size candidates present on both sides are hashed to find exact, name-independent content matches. Files with no potential length match are not prehashed.
5. Hash only **one sequential file stream per selected volume** to avoid excessive random I/O on HDDs and slow USB drives. The UI receives throttled progress notifications; heavy work stays in a worker thread.
6. During copying, hash each source **as it streams to the staging file** (one read pass). Validate size/date before and after. For a source with a previously computed digest, compare the transferred digest against it. Replaced/destination-only originals are checked against their scan metadata and, where known, their digest, then saved in recovery.

**Full verification** is available under **Tools → Settings** and hashes **every file on both sides** before planning changes. Use it when exact equality matters, including possible same-size/same-date modifications. On terabytes of data this can take hours: actual speed is limited by disk I/O and filesystem/drive performance. No invented performance benchmark is claimed.

A Quick check can miss content changes if **both length and modification time remain identical**. A metadata-based check cannot provide the same guarantee as a full content verification. File modifications by other applications during scanning/copying can still cause races; close writers or use filesystem snapshots for critical operations. Because filenames can be duplicated, Quick mode may still need to read significant data if many files share sizes.

## Workflow

1. Enter or browse for a source and destination in **Copy from** and **Copy to**.
2. Select **Compare folders**. Open **Show files** to inspect individual paths and actions.
3. Select **Review and copy** to see what the operation will do, then confirm to apply it.

Open **Tools → Settings** to change the copy or verification mode. Use **Activity** to review warnings and recovery paths or export a JSON report.

## Features and important limits

- **Copy differences:** copy missing/changed paths, skipping files judged identical (see Quick vs Full verification).
- **File-level mirror:** match the source tree and put extra destination files into `.comparator-recovery/<run>/removed`, rather than destroying them. An incomplete scan blocks mirror mode; explicit `MIRROR` confirmation is required.
- **Copy what fits:** prefer smaller complete files within the reported free-space limit, retaining a 32 MiB margin. Free-space reporting may be unavailable or stale.
- **File matches regardless of filename:** BLAKE3 content hashes for feasible same-length candidates.
- **Two-file comparison:** BLAKE3 hashes, optionally a small text diff.
- **Safety:** source stays read-only; reject nested/identical roots; do not follow symlinks; stage new files, sync them, then install without clobbering; save replaced originals to a recovery folder. Cancel stops the current copy operation; completed files stay copied.

**This is not block-level disk cloning.** It does **not** make bootable clones, partition tables, filesystem ownership/ACLs, extended attributes, alternate data streams, sparse allocation, hard-link identity, or timestamp-preserving copies. The recovery folder consumes space; restore from it manually.

## Build and run

Install Rust with Cargo. On Linux, the CI workflow lists native dependencies for Ubuntu. The folder and file pickers use the desktop's file dialog portal.

```sh
cargo test --locked
cargo run --locked --release
```

On macOS, from the repository root:

```sh
./scripts/bundle-macos.sh
open "dist/Comparator Studio.app"
```

The script builds for the Mac's active Rust toolchain architecture, assembles **Comparator Studio.app** with its Finder and Dock icon, and applies an ad hoc signature for local use. The bundle can also be moved into Applications. CI publishes a macOS archive using the same script. It has no Developer ID signature or notarization.

## Source

- `src/core.rs` implements scanning, comparison, copy planning, and recovery.
- `src/ui.rs` implements the native egui interface.
- `make_icon.py` generates the checked-in icon assets using Pillow.
- `.github/workflows/build.yml` builds and tests on Linux, macOS, and Windows.

Licensed under MIT. See `LICENSE`.

Geist is bundled under the SIL Open Font License. See `assets/Geist-OFL.txt`.
