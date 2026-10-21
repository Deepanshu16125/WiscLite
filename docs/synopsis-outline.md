# WiscLite — Project Synopsis Outline (B.Tech CSE Final Year)

1. **Title:** WiscLite: A Key-Value Storage Engine with Key-Value Separation for Reduced Write Amplification
2. **Abstract (≈150 words):** LSM-trees power RocksDB, Cassandra and similar systems, but compaction rewrites values over and over. Implement WiscKey's key-value separation in Rust, compare it against an inline-value baseline, and report measured write amplification (2.06× → 1.10×, with a 16× smaller tree).
3. **Introduction:** key-value stores, why LSM-trees are write-optimised, SSD wear, and the problem statement.
4. **Literature survey:** O'Neil 1996 (LSM-tree), LevelDB and RocksDB, WiscKey (FAST '16), and later work such as HashKV and Titan (TiKV's blob storage). Point out the gap: these ideas are rarely built from scratch in teaching projects.
5. **Objectives:** (a) a correct, crash-safe engine, (b) key-value separation, (c) a measurable comparison with a baseline, (d) a visual dashboard.
6. **System design:** the architecture diagram (README), on-disk formats for the WAL, vlog and sstable, the read and write paths, and the recovery algorithm.
7. **Implementation:** Rust 2024, crates `crc32fast`, `serde_json` and `rand`, a module-by-module walk-through, and the CLI.
8. **Testing:** 13 unit and integration tests covering recovery, torn and corrupt logs, tombstones, ordering and flush.
9. **Results:** the bench table, write-amplification chart, compaction projection, throughput and latency.
10. **Limitations and future work:** compaction, vlog GC, Bloom filters, range scans, concurrency.
11. **Conclusion.**
12. **References:** see README.
