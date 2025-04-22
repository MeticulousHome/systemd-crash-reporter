#!/bin/bash
DIR=$(dirname $0)
PROJECT="$(pwd)/${DIR}"

rustup target add aarch64-unknown-linux-gnu

sudo apt-get update > /dev/null
sudo apt install -y gcc-aarch64-linux-gnu

CONFIG_FILE="$HOME/.cargo/config"

if ! grep -q "\[target.aarch64-unknown-linux-gnu\]" "$CONFIG_FILE" 2>/dev/null || ! grep -q 'linker = "aarch64-linux-gnu-gcc"' "$CONFIG_FILE" 2>/dev/null; then
    mkdir -p "$(dirname "$CONFIG_FILE")"
    {
        echo '[target.aarch64-unknown-linux-gnu]'
        echo 'linker = "aarch64-linux-gnu-gcc"'
    } >> "$CONFIG_FILE"
    ecjo
fi
export TARGET_CC=aarch64-linux-gnu-gcc

cargo build --target aarch64-unknown-linux-gnu --release
cargo build --release