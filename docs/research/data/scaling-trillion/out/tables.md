| component | operation | N range (MEASURED) | exponent k (all) | k (top 4) | class | value at N max (MEASURED) | at 1e12 (PROJECTED) |
|---|---|---|---|---|---|---|---|
| agi5 (agi5) | find_hit p50 | 1e+03 to 2e+08 | 0.39 | 0.638 | O(n) | 41.41 us | 9.5 ms |
| agi5 (agi5) | find_miss p50 | 1e+03 to 2e+08 | 0.304 | 0.449 | sub-linear | 17.68 us | 809.99 us |
| agi5 (agi5) | open p50 | 1e+03 to 2e+08 | 0.3 | 0.32 | sub-linear | 322.46 us | 4.9 ms |
| agi5 (agi5) | open memory | 1e+03 to 2e+08 | 0.166 | 0.162 | sub-linear | 5.0 MB | 19.9 MB |
| agi5 (agi5) | scan_all_status_pass p50 | 1e+03 to 2e+08 | 1.009 | 1.009 | O(n) | 12.7 s | 19.0 h |
| agi5 (agi5) | verify_all p50 | 1e+03 to 2e+08 | 1.043 | 1.002 | O(n) | 1.8 s | 2.6 h |
| commit_graph (commit_graph) | count_reachable_not_from_1000 p50 | 3e+03 to 3e+08 | 1.404 | 1.085 | O(n) | 73.9 s | 6 d |
| commit_graph (commit_graph) | decode_validate p50 | 3e+03 to 3e+08 | 0.997 | 1.011 | O(n) | 17.0 s | 17.3 h |
| commit_graph (commit_graph) | decode_validate memory | 3e+03 to 3e+08 | 1.0 | 1.0 | O(n) | 30.5 GB | 101.8 TB |
| commit_graph (commit_graph) | encode p50 | 3e+03 to 3e+08 | 0.969 | 0.994 | O(n) | 9.0 s | 7.9 h |
| commit_graph (commit_graph) | index_of p50 | 3e+03 to 3e+08 | 0.272 | 0.177 | sub-linear | 2.62 us | 11.00 us |
| commit_graph (commit_graph) | merge_base_fork_distance_1 p50 | 3e+03 to 3e+08 | 0.616 | 0.064 | flat / O(log n) | 303.65 us | 693.20 us |
| commit_graph (commit_graph) | merge_base_fork_distance_100 p50 | 3e+03 to 3e+08 | 0.647 | 0.082 | flat / O(log n) | 884.08 us | 1.8 ms |
| commit_graph (commit_graph) | merge_base_fork_distance_10000 p50 | 3e+03 to 3e+08 | 0.68 | 0.389 | sub-linear | 51.2 ms | 1.2 s |
| commit_graph (commit_graph) | merge_base_fork_distance_1000000 p50 | 3e+03 to 3e+08 | 0.896 | 0.295 | sub-linear | 713.5 ms | 7.8 s |
| commit_graph (commit_graph) | range_ids_1000 p50 | 3e+03 to 3e+08 | 0.53 | 0.43 | sub-linear | 2.4 ms | 77.7 ms |
| commit_graph (commit_graph_build) | build_extend p50 | 1e+03 to 1e+06 | 1.838 | 1.902 | O(n) | 557.1 s | 1672017761 d |
| commit_graph (commit_graph_build) | extend_one_commit p50 | 1e+03 to 1e+06 | 1.192 | 1.25 | O(n) | 1.1 s | 393 d |
| commit_graph (commit_graph_build_fixed) | build_extend p50 | 1e+03 to 1e+07 | 1.185 | 1.184 | O(n) | 71.9 s | 692 d |
| commit_graph (commit_graph_build_fixed) | extend_one_commit p50 | 1e+03 to 1e+07 | 1.145 | 1.248 | O(n) | 546.2 ms | 11 d |
| locks (locks) | acquire_validate_apply p50 | 1e+02 to 1e+07 | 1.115 | 1.145 | O(n) | 7.7 s | 47 d |
| locks (locks) | manifest_decode p50 | 1e+02 to 1e+07 | 1.075 | 1.05 | O(n) | 9.0 s | 19 d |
| locks (locks) | manifest_encode p50 | 1e+02 to 1e+07 | 1.256 | 1.214 | O(n) | 3.3 s | 44 d |
| manifest (manifest) | decode p50 | 1e+02 to 1e+06 | 1.097 | 1.049 | O(n) | 1.7 s | 39 d |
| manifest (manifest) | encode p50 | 1e+02 to 1e+06 | 1.321 | 1.206 | O(n) | 608.1 ms | 122 d |
| manifest (manifest) | sharded_commit_one_zone p50 | 1e+02 to 1e+05 | 0.007 | 0.007 | flat / O(log n) | 230.15 us | 230.15 us |
| manifest (manifest) | sharded_migrate p50 | 1e+02 to 1e+05 | 1.003 | 1.003 | O(n) | 18.2 s | 2207 d |
| manifest (manifest) | sharded_snapshot_all_zones p50 | 1e+02 to 1e+05 | 0.953 | 0.953 | O(n) | 702.8 ms | 38 d |
| manifest (manifest) | single_commit_cas_fs p50 | 1e+02 to 1e+06 | 0.97 | 1.013 | O(n) | 2.3 s | 32 d |
| manifest (manifest) | single_commit_cas_mem p50 | 1e+02 to 1e+06 | 1.095 | 1.057 | O(n) | 2.2 s | 57 d |
| pack_index (pack_index) | lookup_hit p50 | 1e+03 to 1e+09 | 0.25 | 0.173 | sub-linear | 1.98 us | 6.56 us |
| pack_index (pack_index) | lookup_hit_cold p50 | 1e+03 to 1e+09 | 0.295 | 0.11 | flat / O(log n) | 3.86 us | 6.82 us |
| pack_index (pack_index) | lookup_miss_near_collision p50 | 1e+03 to 1e+09 | 0.224 | 0.179 | sub-linear | 1.85 us | 6.37 us |
| pack_index (pack_index) | lookup_miss_random p50 | 1e+03 to 1e+09 | 0.247 | 0.179 | sub-linear | 1.87 us | 6.46 us |
| pack_index (pack_index) | open p50 | 1e+03 to 1e+09 | 0.085 | 0.006 | flat / O(log n) | 14.72 us | 23.39 us |
| pack_index (pack_index) | verify_full p50 | 1e+03 to 1e+08 | 0.964 | 0.998 | O(n) | 4.0 s | 10.8 h |
| pack_index (pack_index_mmap_warm) | lookup_hit p50 | 1e+06 to 1e+09 | 0.444 | 0.444 | sub-linear | 12.62 us | 270.22 us |
| pack_index (pack_index_mmap_warm) | lookup_hit_cold p50 | 1e+06 to 1e+09 | 0.472 | 0.472 | sub-linear | 27.14 us | 706.03 us |
| pack_index (pack_index_mmap_warm) | lookup_miss_near_collision p50 | 1e+06 to 1e+09 | 0.375 | 0.375 | sub-linear | 7.59 us | 101.41 us |
| pack_index (pack_index_mmap_warm) | lookup_miss_random p50 | 1e+06 to 1e+09 | 0.403 | 0.403 | sub-linear | 9.00 us | 145.54 us |
| pack_index (pack_index_mmap_warm) | open p50 | 1e+06 to 1e+09 | -0.034 | -0.034 | flat / O(log n) | 16.49 us | 16.49 us |
| pack_index (pack_index_mmap_warm) | verify_full p50 | 1e+06 to 1e+08 | 0.846 | 0.846 | O(n) | 2.0 s | 1.3 h |
| path_filter (pfilter) | load_parse p50 | 1e+03 to 1e+08 | 0.969 | 1.002 | O(n) | 4.2 s | 12.0 h |
| path_filter (pfilter) | load_parse memory | 1e+03 to 1e+08 | 0.958 | 0.998 | O(n) | 6.5 GB | 64.0 TB |
| path_filter (pfilter) | lookup p50 | 1e+03 to 1e+08 | 0.222 | 0.246 | sub-linear | 1.72 us | 16.67 us |
| reach_bitmap (reach) | load_verify p50 | 1e+03 to 1e+09 | 1.001 | 1.009 | O(n) | 19.1 s | 5.7 h |
| reach_bitmap (reach) | load_verify memory | 1e+03 to 1e+09 | 1.006 | 1.0 | O(n) | 32.3 GB | 32.3 TB |
| reach_bitmap (reach) | member_lookup p50 | 1e+03 to 1e+09 | 0.19 | 0.211 | sub-linear | 1.99 us | 8.52 us |
| reach_bitmap (reach) | member_miss p50 | 1e+03 to 1e+09 | 0.195 | 0.212 | sub-linear | 1.94 us | 8.39 us |
| reach_bitmap (reach) | roots_within_256 p50 | 1e+03 to 1e+09 | -0.087 | -0.009 | flat / O(log n) | 0.21 us | 0.21 us |
