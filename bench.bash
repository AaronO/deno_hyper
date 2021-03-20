#!/bin/bash

env RUSTFLAGS="-C debuginfo=1" cargo build --release
# perf record -c 40000 -i -g target/release/dyper_ops --perf_basic_prof &
target/release/dyper_ops &
pid=$!
sleep .5
../deno/third_party/prebuilt/mac/wrk -d 20s --latency http://127.0.0.1:4000/
kill $pid
