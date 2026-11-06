
#### deep N=1,000,000 (dataset `e2e`)

| step | vcs wall | vcs peak RSS | git wall | git peak RSS |
|---|---|---|---|---|
| import | 54.03 s | 2.18 GiB | 81.72 s | 860 MiB |
| status_first | 0.01 s | 13 MiB | 0.02 s | 6 MiB |
| status_second | 0.01 s | 13 MiB | 0.01 s | 6 MiB |
| log_n20 | 0.00 s | 50 MiB | 0.00 s | 12 MiB |
| rev_list_head | 1.82 s | 254 MiB | 4.22 s | 526 MiB |
| tag_create | 0.03 s | 18 MiB | 0.00 s | 5 MiB |
| add_one_file | 0.01 s | 12 MiB | 0.01 s | 5 MiB |
| commit_one_file | 0.01 s | 29 MiB | 0.06 s | 36 MiB |
| publish_first | 39.37 s | 10.52 GiB | 49.38 s | 1.33 GiB |
| publish_incremental | 2.17 s | 345 MiB | 0.96 s | 389 MiB |
| clone_full | 87.38 s | 2.65 GiB | 60.39 s | 1.26 GiB |
| gc_first | 46.53 s | 5.49 GiB | 40.22 s | 1.36 GiB |
| gc_second | 4.47 s | 867 MiB | 33.66 s | 1.38 GiB |
| fsck | 198 s | 5.90 GiB | 43.67 s | 1015 MiB |
| checkout | n/a | n/a | 0.03 s | 92 MiB |
| commit_graph_write | n/a | n/a | 7.07 s | 498 MiB |

- vcs disk_after_import: 4.25 GiB

- vcs disk_remote: 4.10 GiB

- vcs disk_after_gc: 4.42 GiB

- git disk_after_import: 0.76 GiB

- git disk_remote: 0.39 GiB

- git disk_after_gc: 0.45 GiB
