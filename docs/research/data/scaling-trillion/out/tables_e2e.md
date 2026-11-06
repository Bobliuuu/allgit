
#### deep N=10,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 0.51 s | 70 MiB | 1.22 s | 13 MiB |
| status_first | 0.01 s | 11 MiB | 0.01 s | 6 MiB |
| status_second | 0.01 s | 11 MiB | 0.01 s | 6 MiB |
| log_n20 | 0.00 s | 10 MiB | 0.00 s | 6 MiB |
| rev_list_head | 0.01 s | 11 MiB | 0.06 s | 10 MiB |
| tag_create | 0.01 s | 10 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 10 MiB | 0.01 s | 5 MiB |
| commit_one_file | 0.01 s | 11 MiB | 0.01 s | 8 MiB |
| publish_first | 0.36 s | 163 MiB | 0.46 s | 51 MiB |
| publish_incremental | 0.03 s | 13 MiB | 0.03 s | 9 MiB |
| clone_full | 0.51 s | 121 MiB | 0.56 s | 50 MiB |
| gc_first | 0.36 s | 68 MiB | 0.31 s | 51 MiB |
| gc_second | 0.06 s | 17 MiB | 0.26 s | 51 MiB |
| fsck | 1.17 s | 177 MiB | 0.41 s | 48 MiB |
| checkout | n/a | n/a | 0.03 s | 10 MiB |
| commit_graph_write | n/a | n/a | 0.06 s | 10 MiB |

- vcs disk_after_import: 0.04 GiB

- vcs disk_remote: 0.04 GiB

- vcs disk_after_gc: 0.04 GiB

- git disk_after_import: 0.01 GiB

- git disk_remote: 0.00 GiB

- git disk_after_gc: 0.00 GiB

#### deep N=100,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 7.47 s | 431 MiB | 8.42 s | 85 MiB |
| status_first | 0.02 s | 11 MiB | 0.01 s | 6 MiB |
| status_second | 0.01 s | 11 MiB | 0.01 s | 6 MiB |
| log_n20 | 0.01 s | 11 MiB | 0.00 s | 9 MiB |
| rev_list_head | 0.11 s | 29 MiB | 0.41 s | 58 MiB |
| tag_create | 0.03 s | 13 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 10 MiB | 0.01 s | 5 MiB |
| commit_one_file | 0.01 s | 13 MiB | 0.01 s | 15 MiB |
| publish_first | 3.84 s | 1.32 GiB | 4.47 s | 236 MiB |
| publish_incremental | 0.21 s | 40 MiB | 0.11 s | 43 MiB |
| clone_full | 6.47 s | 808 MiB | 5.87 s | 226 MiB |
| gc_first | 4.27 s | 579 MiB | 3.47 s | 238 MiB |
| gc_second | 0.41 s | 80 MiB | 2.62 s | 238 MiB |
| fsck | 14.18 s | 1.66 GiB | 4.17 s | 202 MiB |
| checkout | n/a | n/a | 0.03 s | 16 MiB |
| commit_graph_write | n/a | n/a | 0.51 s | 55 MiB |

- vcs disk_after_import: 0.42 GiB

- vcs disk_remote: 0.41 GiB

- vcs disk_after_gc: 0.44 GiB

- git disk_after_import: 0.08 GiB

- git disk_remote: 0.04 GiB

- git disk_after_gc: 0.05 GiB

#### deep N=1,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 575 s | 2.18 GiB | 82.28 s | 860 MiB |
| status_first | 0.02 s | 12 MiB | 0.02 s | 6 MiB |
| status_second | 0.01 s | 12 MiB | 0.00 s | 6 MiB |
| log_n20 | 0.00 s | 50 MiB | 0.00 s | 12 MiB |
| rev_list_head | 1.92 s | 254 MiB | 4.27 s | 526 MiB |
| tag_create | 0.03 s | 18 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 12 MiB | 0.00 s | 5 MiB |
| commit_one_file | 0.01 s | 27 MiB | 0.01 s | 35 MiB |
| publish_first | 40.92 s | 10.53 GiB | 49.88 s | 1.33 GiB |
| publish_incremental | 2.17 s | 345 MiB | 0.96 s | 389 MiB |
| clone_full | 91.18 s | 2.63 GiB | 61.24 s | 1.26 GiB |
| gc_first | 50.53 s | 5.49 GiB | 39.87 s | 1.36 GiB |
| gc_second | 4.47 s | 867 MiB | 33.46 s | 1.38 GiB |
| fsck | 202 s | 15.59 GiB | 43.62 s | 1015 MiB |
| checkout | n/a | n/a | 0.03 s | 92 MiB |
| commit_graph_write | n/a | n/a | 7.02 s | 498 MiB |

- vcs disk_after_import: 4.25 GiB

- vcs disk_remote: 4.10 GiB

- vcs disk_after_gc: 4.42 GiB

- git disk_after_import: 0.76 GiB

- git disk_remote: 0.39 GiB

- git disk_after_gc: 0.45 GiB

#### slice N=1,000,000 (dataset `slice`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| add_all | 7.21 s | 1.05 GiB | n/a | n/a |
| commit_all | 0.63 s | 293 MiB | n/a | n/a |
| publish_all | 5.53 s | 1.64 GiB | n/a | n/a |
| clone_all | 12.70 s | 1.38 GiB | 22.73 s | 684 MiB |
| clone_one_zone | 0.39 s | 252 MiB | n/a | n/a |
| clone_one_dir | n/a | n/a | 0.87 s | 562 MiB |

- files in the cloned slice: 10,000

#### slice N=10,000,000 (dataset `slice`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| add_all | 81.22 s | 6.58 GiB | n/a | n/a |
| commit_all | 6.05 s | 2.74 GiB | n/a | n/a |
| publish_all | 62.44 s | 13.05 GiB | n/a | n/a |
| clone_all | 177 s | 8.69 GiB | 252 s | 6.68 GiB |
| clone_one_zone | 1.03 s | 990 MiB | n/a | n/a |
| clone_one_dir | n/a | n/a | 9.79 s | 5.58 GiB |

- files in the cloned slice: 10,000

#### wide N=10,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 0.21 s | 22 MiB | 0.16 s | 8 MiB |
| status_first | 0.03 s | 12 MiB | 0.06 s | 7 MiB |
| status_second | 0.03 s | 12 MiB | 0.06 s | 7 MiB |
| log_n20 | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| rev_list_head | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| tag_create | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 11 MiB | 0.06 s | 6 MiB |
| commit_one_file | 0.01 s | 12 MiB | 0.02 s | 7 MiB |
| publish_first | 0.03 s | 15 MiB | 0.06 s | 8 MiB |
| publish_incremental | 0.01 s | 11 MiB | 0.03 s | 6 MiB |
| clone_full | 0.16 s | 22 MiB | 0.16 s | 8 MiB |
| gc_first | 0.03 s | 16 MiB | 0.06 s | 10 MiB |
| gc_second | 0.03 s | 15 MiB | 0.06 s | 10 MiB |
| fsck | 0.03 s | 17 MiB | 0.03 s | 9 MiB |
| checkout | n/a | n/a | 0.16 s | 7 MiB |
| commit_graph_write | n/a | n/a | 0.00 s | 5 MiB |

- vcs disk_after_import: 0.00 GiB

- vcs disk_remote: 0.00 GiB

- vcs disk_after_gc: 0.00 GiB

- git disk_after_import: 0.00 GiB

- git disk_remote: 0.00 GiB

- git disk_after_gc: 0.00 GiB

#### wide N=100,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 1.62 s | 135 MiB | 0.92 s | 31 MiB |
| status_first | 0.21 s | 41 MiB | 0.16 s | 27 MiB |
| status_second | 0.21 s | 36 MiB | 0.16 s | 27 MiB |
| log_n20 | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| rev_list_head | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| tag_create | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 15 MiB | 0.06 s | 27 MiB |
| commit_one_file | 0.11 s | 37 MiB | 0.06 s | 28 MiB |
| publish_first | 0.26 s | 68 MiB | 0.46 s | 34 MiB |
| publish_incremental | 0.01 s | 11 MiB | 0.06 s | 17 MiB |
| clone_full | 1.57 s | 131 MiB | 1.26 s | 31 MiB |
| gc_first | 0.31 s | 88 MiB | 0.31 s | 59 MiB |
| gc_second | 0.26 s | 69 MiB | 0.31 s | 58 MiB |
| fsck | 0.36 s | 94 MiB | 0.31 s | 42 MiB |
| checkout | n/a | n/a | 1.36 s | 26 MiB |
| commit_graph_write | n/a | n/a | 0.00 s | 6 MiB |

- vcs disk_after_import: 0.02 GiB

- vcs disk_remote: 0.01 GiB

- vcs disk_after_gc: 0.03 GiB

- git disk_after_import: 0.02 GiB

- git disk_remote: 0.01 GiB

- git disk_after_gc: 0.02 GiB

#### wide N=1,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 18.09 s | 1.29 GiB | 15.28 s | 282 MiB |
| status_first | 2.22 s | 350 MiB | 1.11 s | 241 MiB |
| status_second | 0.92 s | 289 MiB | 1.17 s | 241 MiB |
| log_n20 | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| rev_list_head | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| tag_create | 0.00 s | 10 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.06 s | 28 MiB | 0.61 s | 241 MiB |
| commit_one_file | 0.77 s | 290 MiB | 0.61 s | 242 MiB |
| publish_first | 3.42 s | 568 MiB | 5.77 s | 319 MiB |
| publish_incremental | 0.01 s | 13 MiB | 0.51 s | 148 MiB |
| clone_full | 18.04 s | 1.05 GiB | 13.38 s | 289 MiB |
| gc_first | 4.62 s | 736 MiB | 4.42 s | 525 MiB |
| gc_second | 2.47 s | 583 MiB | 4.42 s | 510 MiB |
| fsck | 4.62 s | 725 MiB | 3.22 s | 396 MiB |
| checkout | n/a | n/a | 14.33 s | 226 MiB |
| commit_graph_write | n/a | n/a | 0.00 s | 6 MiB |

- vcs disk_after_import: 0.18 GiB

- vcs disk_remote: 0.10 GiB

- vcs disk_after_gc: 0.27 GiB

- git disk_after_import: 0.16 GiB

- git disk_remote: 0.08 GiB

- git disk_after_gc: 0.16 GiB

#### wide N=10,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 197 s | 10.97 GiB | 1694 s (rc -9) | n/a |
| status_first | 23.42 s | 3.22 GiB | 0.00 s | 4 MiB |
| status_second | 9.58 s | 2.77 GiB | 0.00 s | 5 MiB |
| log_n20 | 0.00 s | 12 MiB | 0.00 s (rc 128) | 4 MiB |
| rev_list_head | 0.00 s | 11 MiB | 0.00 s (rc 128) | 4 MiB |
| tag_create | 0.00 s | 14 MiB | 0.00 s (rc 128) | 4 MiB |
| add_one_file | 0.01 s | 48 MiB | n/a | n/a |
| commit_one_file | 7.62 s | 2.77 GiB | n/a | n/a |
| publish_first | 35.43 s | 3.55 GiB | n/a | n/a |
| publish_incremental | 0.02 s | 35 MiB | n/a | n/a |
| clone_full | 222 s | 8.73 GiB | n/a | n/a |
| gc_first | 51.53 s | 6.04 GiB | n/a | n/a |
| gc_second | 26.45 s | 5.26 GiB | n/a | n/a |
| fsck | 55.49 s | 6.00 GiB | n/a | n/a |
| checkout | n/a | n/a | 0.00 s (rc 1) | 4 MiB |

- vcs disk_after_import: 1.87 GiB

- vcs disk_remote: 1.01 GiB

- vcs disk_after_gc: 2.74 GiB

- git disk_after_import: 0.17 GiB
