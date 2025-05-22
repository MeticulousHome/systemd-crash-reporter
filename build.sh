#!/bin/bash
set -e

rustup target add aarch64-unknown-linux-gnu

apt-get update > /dev/null
apt install -y gcc-aarch64-linux-gnu

CONFIG_FILE="$HOME/.cargo/config"

if ! grep -q "\[target.aarch64-unknown-linux-gnu\]" "$CONFIG_FILE" 2>/dev/null || ! grep -q 'linker = "aarch64-linux-gnu-gcc"' "$CONFIG_FILE" 2>/dev/null; then
    mkdir -p "$(dirname "$CONFIG_FILE")"
    {
        echo '[target.aarch64-unknown-linux-gnu]'
        echo 'linker = "aarch64-linux-gnu-gcc"'
    } >> "$CONFIG_FILE"
    echo
fi
export TARGET_CC=aarch64-linux-gnu-gcc

cd "/systemd-crash-reporter"
cargo build --target aarch64-unknown-linux-gnu --release
cargo build --release