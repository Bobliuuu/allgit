# Large-file benchmark (box d, c6i.4xlarge 16 vCPU, ext4 on EBS, warm page cache)

Single file, v1 random, v2 = v1 with 3 x 1 MiB overwritten. CPU = user+sys seconds (primary), wall seconds, peak RSS MiB. `git` and `git-lfs` rows were run while other jobs shared the box (load1 up to ~10, so their wall times are noisy; CPU is robust). vcs before/after runs started at load1 0.0 / 4.7 (the after run began right after a build). One run each, no medians. git-lfs push/clone/fetch rows are NOT comparable (standalone file agent; the clone did not smudge) and are omitted; vcs fetch rows are omitted (the harness fetch step transferred nothing). `git clone` of a local path hardlinks objects, so its clone row is not a transfer cost. Raw TSVs: ~/rt/bigdata/{base,cmp,cmp4}.tsv on box d.


## 256 MiB

| step | git 2.43 | git-lfs 3.6.1 | vcs before | vcs after |
|---|---|---|---|---|
| add (cpu/wall/rssMiB) | 7.60 / 7.61 / 260 | 0.96 / 0.59 / 15 | 0.74 / 1.62 / 522 | 0.71 / 1.17 / 83 |
| status_touch (cpu/wall/rssMiB) | 0.89 / 0.90 / 260 | 0.98 / 0.61 / 15 | 0.39 / 0.39 / 265 | 0.25 / 0.03 / 100 |
| status_edit1 (cpu/wall/rssMiB) | 0.79 / 0.80 / 260 | 3.90 / 2.36 / 16 | 0.38 / 0.39 / 265 | 0.20 / 0.02 / 92 |
| push1 (cpu/wall/rssMiB) | 16.15 / 15.23 / 517 | n/a | 1.25 / 1.76 / 795 | 0.76 / 2.11 / 190 |
| add2 (cpu/wall/rssMiB) | 7.58 / 7.59 / 260 | 0.96 / 0.58 / 15 | 0.67 / 0.69 / 269 | 0.29 / 0.13 / 97 |
| switch_v1 (cpu/wall/rssMiB) | 0.24 / 0.24 / 260 | 0.63 / 0.34 / 260 | 0.73 / 1.75 / 266 | 0.55 / 0.12 / 115 |
| switch_v2 (cpu/wall/rssMiB) | 1.04 / 1.05 / 260 | 1.59 / 0.93 / 260 | 0.73 / 2.05 / 266 | 0.55 / 0.12 / 115 |
| clone (cpu/wall/rssMiB) | 0.21 / 0.34 / 261 | n/a | 1.17 / 2.43 / 608 | 0.77 / 1.90 / 347 |
| store_after_v2_bytes | 512 MiB | 768 MiB | 260 MiB | 265 MiB |
| remote_bytes | 512 MiB | 512 MiB | 260 MiB | 265 MiB |

## 1024 MiB

| step | git 2.43 | git-lfs 3.6.1 | vcs before | vcs after |
|---|---|---|---|---|
| add (cpu/wall/rssMiB) | 31.44 / 40.59 / 5 | 3.82 / 2.28 / 15 | 3.00 / 9.39 / 1296 | 2.69 / 7.71 / 114 |
| status_touch (cpu/wall/rssMiB) | 27.48 / 27.49 / 5 | 7.89 / 7.99 / 28 | 1.58 / 1.58 / 1036 | 0.94 / 0.07 / 126 |
| status_edit1 (cpu/wall/rssMiB) | 30.23 / 30.24 / 5 | 7.81 / 8.78 / 31 | 1.57 / 1.58 / 1035 | 0.53 / 0.04 / 119 |
| push1 (cpu/wall/rssMiB) | 36.47 / 31.70 / 1029 | n/a | 4.87 / 9.67 / 3133 | 2.86 / 8.31 / 258 |
| add2 (cpu/wall/rssMiB) | 31.34 / 38.14 / 4 | 3.91 / 3.53 / 15 | 2.74 / 2.76 / 1042 | 0.91 / 0.19 / 134 |
| switch_v1 (cpu/wall/rssMiB) | 0.87 / 0.88 / 1029 | 2.51 / 1.50 / 1027 | 1.38 / 8.55 / 1036 | 1.13 / 0.33 / 62 |
| switch_v2 (cpu/wall/rssMiB) | 30.21 / 30.22 / 1029 | 6.47 / 5.50 / 1028 | 2.97 / 10.09 / 1037 | 2.04 / 0.40 / 152 |
| clone (cpu/wall/rssMiB) | 0.84 / 0.97 / 1029 | n/a | 4.03 / 9.60 / 1933 | 3.00 / 8.01 / 439 |
| store_after_v2_bytes | 2048 MiB | 3072 MiB | 1033 MiB | 1039 MiB |
| remote_bytes | 2048 MiB | 2048 MiB | 1033 MiB | 1039 MiB |

## 4096 MiB

| step | git 2.43 | git-lfs 3.6.1 | vcs before | vcs after |
|---|---|---|---|---|
| add (cpu/wall/rssMiB) | 126.39 / 144.48 / 5 | 16.00 / 23.06 / 16 | 12.10 / 88.84 / 4396 | 9.86 / 36.21 / 228 |
| status_touch (cpu/wall/rssMiB) | 111.46 / 111.47 / 5 | 16.60 / 32.11 / 16 | 6.17 / 6.17 / 4119 | 3.40 / 0.23 / 152 |
| status_edit1 (cpu/wall/rssMiB) | 125.96 / 125.99 / 4 | 15.52 / 10.09 / 16 | 6.17 / 6.17 / 4119 | 1.70 / 0.12 / 144 |
| push1 (cpu/wall/rssMiB) | 162.19 / 145.82 / 4076 | n/a | 19.80 / 41.00 / 12459 | 10.93 / 33.80 / 485 |
| add2 (cpu/wall/rssMiB) | 137.61 / 145.99 / 5 | 15.49 / 9.12 / 16 | 10.76 / 10.78 / 4137 | 3.43 / 0.27 / 162 |
| switch_v1 (cpu/wall/rssMiB) | 3.52 / 6.21 / 4101 | 9.90 / 26.76 / 4100 | 7.80 / 67.65 / 4120 | 4.63 / 1.88 / 70 |
| switch_v2 (cpu/wall/rssMiB) | 116.00 / 175.35 / 3881 | 26.36 / 35.53 / 4100 | 12.63 / 52.19 / 4125 | 7.24 / 39.78 / 179 |
| clone (cpu/wall/rssMiB) | 3.98 / 47.39 / 4099 | n/a | 16.36 / 83.82 / 4605 | 10.21 / 66.90 / 627 |
| store_after_v2_bytes | 8194 MiB | 12288 MiB | 4123 MiB | 4113 MiB |
| remote_bytes | 8194 MiB | 8192 MiB | 4123 MiB | 4113 MiB |
