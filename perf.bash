#!/bin/bash

env RUSTFLAGS="-C debuginfo=1" cargo build --release
perf record -c 40000 -i -g target/release/dyper_ops --perf_basic_prof &
pid=$!
sleep .5
../deno/third_party/prebuilt/linux64/wrk -d 10s --latency http://127.0.0.1:4000/
kill $pid
