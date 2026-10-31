# Environment for building the lab on the Linux box (rocksdb needs libclang; installed via `pip install --target ~/rt/libclang libclang`).
. ~/.cargo/env
export LIBCLANG_PATH=$HOME/rt/libclang/clang/native
export BINDGEN_EXTRA_CLANG_ARGS="-I/usr/lib/gcc/x86_64-linux-gnu/12/include"
