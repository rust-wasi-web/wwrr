#!/bin/bash

set -e
cd "$( dirname "${BASH_SOURCE[0]}" )"

rm -rf pkg

export WBG_PKG=wwrr-
#export RUSTFLAGS="-Z stack-protector=all"

cargo +nightly build --release --package wwrr
cargo +nightly run --target x86_64-unknown-linux-gnu --bin bindgen-cli -- --target web --out-dir pkg target/wasm32-unknown-unknown/release/wwrr.wasm
rm -f pkg/wwrr_bg.wasm.d.ts
