#!/bin/bash
set -e
cargo build --release
./target/release/belch_proxy_beta
