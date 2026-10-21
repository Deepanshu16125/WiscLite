# Viva notes — WiscLite

## Write amplification, simply
> "If you save **1 MB** and the disk actually writes **10 MB**, write amplification is **10×**."

Analogy: a librarian keeps a shelf sorted. Every time new books arrive, she re-sorts by **taking the books off and putting them back**. If she moves the *whole books* each time, that's a lot of lifting. WiscKey says: keep the books in a warehouse in arrival order (the **value log**) and only re-sort **index cards** with "aisle and shelf number" on them (the **LSM tree**). Re-sorting cards is cheap.

## My numbers (say these)
- 50,000 puts of 1 KB each, about 52 MB of real data, with compaction on.
- Baseline wrote 159 MB, a **WA of 3.06×**: the WAL, the flush and then compaction each rewrite every value.
- WiscKey wrote 57 MB, a **WA of 1.10×**: each value goes to the vlog once, and compaction moves only 21-byte pointers.
- At 500,000 puts the gap grows: **4.55× vs 1.16×**. The baseline spent 1.3 GB on compaction, WiscKey 34 MB.
- Bloom filters skip tables for missing keys, with a measured false-positive rate of about 0.8% (10 bits per key).
- The honest cost: WiscKey range scans are slower (11.4 ms vs 3.3 ms for 3,266 rows), and it needs a GC step (13.8 MB reclaimed after overwriting 25% of keys).

## Likely questions
- **Why is your baseline 3–4.5×, not 10–50×?** My dataset is small (a few tiers) and I use size-tiered compaction. Production stores with many levels rewrite data more times, and the gap grows with size, as my 50k vs 500k runs show.
- **What does WiscKey cost?** A get needs one extra read (index → vlog). Range scans become random reads, which the paper hides with parallel prefetch on SSDs. Deleted or overwritten values also need garbage collection in the vlog.
- **How do you survive a crash?** Every write goes to a CRC32-checksummed WAL before the memtable. On open, the WAL is replayed, stopping at the first bad checksum (a torn write). The vlog is written *before* the WAL entry that points to it.
- **How do deletes work?** By writing a tombstone. Reads go newest to oldest, so the tombstone hides older versions. The test is `tombstones_shadow_older_tables`.
- **Why sorted tables plus binary search?** Sorted keys give O(log n) lookups with a small index and make merging cheap.
- **Why Rust?** Memory safety without a garbage collector, predictable performance, and the same language as my CodeCage project.
- **Why CRC32?** It's cheap and catches torn or partial writes and bit flips. It isn't cryptographic, and doesn't need to be.

## 90-second demo
1. `cargo test --release` → 22 passed.
2. `wisclite put name Dee` → `get name` → `del name` → `get name` gives *(not found)*.
3. `wisclite bench` → point at the two WA numbers.
4. Open `metrics.html` → "Where the bytes go" → "Add compaction, and it diverges".
