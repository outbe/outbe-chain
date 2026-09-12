#!/bin/sh
set -eu
bench_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
seal_prefix=${1:?Usage: sh run_seal.sh /path/to/SEAL-4.4-install}
c++ -O3 -std=c++17 -I"$seal_prefix/include/SEAL-4.4" \
  "$bench_dir/seal_bench.cpp" "$seal_prefix/lib/libseal-4.4.a" -o "$bench_dir/seal_bench"
"$bench_dir/seal_bench" > "$bench_dir/seal_results.json"

