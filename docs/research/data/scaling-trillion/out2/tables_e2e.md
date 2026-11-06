
#### deep N=1,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 54.03 s | 2.18 GiB | 82.32 s | 860 MiB |
| status_first | 0.02 s | 13 MiB | 0.01 s | 6 MiB |
| status_second | 0.01 s | 13 MiB | 0.01 s | 6 MiB |
| log_n20 | 0.00 s | 50 MiB | 0.00 s | 12 MiB |
| rev_list_head | 1.92 s | 254 MiB | 4.27 s | 526 MiB |
| tag_create | 0.03 s | 18 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 12 MiB | 0.01 s | 5 MiB |
| commit_one_file | 0.01 s | 25 MiB | 0.01 s | 36 MiB |
| publish_first | 39.97 s | 10.54 GiB | 49.18 s | 1.34 GiB |
| publish_incremental | 2.07 s | 345 MiB | 0.96 s | 389 MiB |
| clone_full | 85.28 s | 2.63 GiB | 61.04 s | 1.26 GiB |
| gc_first | 47.93 s | 5.49 GiB | 40.12 s | 1.36 GiB |
| gc_second | 4.47 s | 867 MiB | 33.51 s | 1.38 GiB |
| fsck | 198 s | 5.89 GiB | 43.32 s | 1016 MiB |
| checkout | n/a | n/a | 0.03 s | 92 MiB |
| commit_graph_write | n/a | n/a | 7.17 s | 498 MiB |

- vcs disk_after_import: 4.25 GiB

- vcs disk_remote: 4.10 GiB

- vcs disk_after_gc: 4.42 GiB

- git disk_after_import: 0.76 GiB

- git disk_remote: 0.39 GiB

- git disk_after_gc: 0.45 GiB

#### deep N=5,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 285 s | 8.71 GiB | 424 s | 4.09 GiB |
| status_first | 0.02 s | 15 MiB | 0.01 s | 6 MiB |
| status_second | 0.01 s | 15 MiB | 0.01 s | 6 MiB |
| log_n20 | 0.00 s | 72 MiB | 0.00 s | 17 MiB |
| rev_list_head | 12.33 s | 1.07 GiB | 23.19 s | 2.65 GiB |
| tag_create | 0.11 s | 52 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 12 MiB | 0.01 s | 6 MiB |
| commit_one_file | 0.01 s | 33 MiB | 0.01 s | 67 MiB |
| publish_first | 205 s | 50.97 GiB | 268 s | 7.22 GiB |
| publish_incremental | 12.33 s | 1.54 GiB | 5.02 s | 1.88 GiB |
| clone_full | 606 s | 11.30 GiB | 322 s | 6.82 GiB |
| gc_first | 262 s | 26.48 GiB | 232 s | 6.49 GiB |
| gc_second | 25.15 s | 4.19 GiB | 198 s | 7.44 GiB |
| fsck | 1296 s | 28.35 GiB | 230 s | 5.22 GiB |
| checkout | n/a | n/a | 0.03 s | 311 MiB |
| commit_graph_write | n/a | n/a | 44.32 s | 2.47 GiB |

- vcs disk_after_import: 21.26 GiB

- vcs disk_remote: 20.49 GiB

- vcs disk_after_gc: 22.12 GiB

- git disk_after_import: 3.79 GiB

- git disk_remote: 1.93 GiB

- git disk_after_gc: 2.28 GiB

#### deep N=10,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 590 s | 17.32 GiB | 905 s | 7.77 GiB |
| status_first | 0.02 s | 15 MiB | 0.02 s | 7 MiB |
| status_second | 0.01 s | 15 MiB | 0.01 s | 7 MiB |
| log_n20 | 0.00 s | 90 MiB | 0.00 s | 22 MiB |
| rev_list_head | 29.10 s | 2.12 GiB | 49.38 s | 5.36 GiB |
| tag_create | 0.16 s | 95 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 14 MiB | 0.00 s | 6 MiB |
| commit_one_file | 0.01 s | 43 MiB | 0.01 s | 93 MiB |
| publish_first | 405 s | 101.18 GiB | 576 s | 14.67 GiB |
| publish_incremental | 75.41 s | 3.04 GiB | 10.38 s | 3.84 GiB |
| clone_full | 1596 s | 21.48 GiB | 674 s | 11.77 GiB |
| gc_first | 602 s | 52.86 GiB | 511 s | 12.71 GiB |
| gc_second | 53.98 s | 8.34 GiB | 449 s | 13.02 GiB |
| fsck | 2926 s | 56.47 GiB | 479 s | 9.50 GiB |
| checkout | n/a | n/a | 0.06 s | 514 MiB |
| commit_graph_write | n/a | n/a | 94.29 s | 4.87 GiB |

- vcs disk_after_import: 42.52 GiB

- vcs disk_remote: 40.99 GiB

- vcs disk_after_gc: 44.25 GiB

- git disk_after_import: 7.65 GiB

- git disk_remote: 4.01 GiB

- git disk_after_gc: 4.70 GiB

#### wide N=30,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 593 s | 34.96 GiB | n/a | n/a |
| status_first | 72.87 s | 9.58 GiB | n/a | n/a |
| status_second | 28.35 s | 8.34 GiB | n/a | n/a |
| log_n20 | 0.00 s | 12 MiB | n/a | n/a |
| rev_list_head | 0.00 s | 12 MiB | n/a | n/a |
| tag_create | 0.00 s | 15 MiB | n/a | n/a |
| add_one_file | 0.01 s | 56 MiB | n/a | n/a |
| commit_one_file | 20.99 s | 8.34 GiB | n/a | n/a |
| publish_first | 110 s | 11.10 GiB | n/a | n/a |
| publish_incremental | 0.06 s | 57 MiB | n/a | n/a |
| clone_full | 764 s | 27.17 GiB | n/a | n/a |
| gc_first | 214 s | 17.31 GiB | n/a | n/a |
| gc_second | 83.32 s | 14.50 GiB | n/a | n/a |
| fsck | 223 s | 15.52 GiB | n/a | n/a |

- vcs disk_after_import: 5.68 GiB

- vcs disk_remote: 3.05 GiB

- vcs disk_after_gc: 8.21 GiB

#### wide N=100,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 2327 s | 100.33 GiB | n/a | n/a |
| status_first | 1053 s | 32.15 GiB | n/a | n/a |
| status_second | 183 s | 27.80 GiB | n/a | n/a |
| log_n20 | 0.01 s | 10 MiB | n/a | n/a |
| rev_list_head | 0.00 s | 9 MiB | n/a | n/a |
| tag_create | 0.01 s | 9 MiB | n/a | n/a |
| add_one_file | 0.01 s | 26 MiB | n/a | n/a |
| commit_one_file | 74.16 s | 27.80 GiB | n/a | n/a |
| publish_first | 702 s | 27.44 GiB | n/a | n/a |
| publish_incremental | 0.11 s | 42 MiB | n/a | n/a |
| clone_full | 4631 s | 74.91 GiB | n/a | n/a |
| gc_first | 846 s | 52.80 GiB | n/a | n/a |
| gc_second | 428 s | 48.36 GiB | n/a | n/a |
| fsck | 1108 s | 47.68 GiB | n/a | n/a |

- vcs disk_after_import: 19.04 GiB

- vcs disk_remote: 10.19 GiB
