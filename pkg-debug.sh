#!/bin/bash

set -e
cd "$( dirname "${BASH_SOURCE[0]}" )"

rm -rf pkg

cargo build --package wwrr
cargo run --quiet --release --target x86_64-unknown-linux-gnu --bin bindgen-cli -- --target web --debug --keep-debug --out-dir pkg target/wasm32-unknown-unknown/debug/wwrr.wasm
rm -f pkg/wwrr_bg.wasm.d.ts
