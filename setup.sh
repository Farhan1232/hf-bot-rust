#!/usr/bin/env bash
#
# One-time setup: installs Rust (if needed), builds the bot, and creates .env.
# Run this ONCE on your Ubuntu server:   ./setup.sh
#
set -e
cd "$(dirname "$0")"

echo "==============================================="
echo " Binance HF Trading Bot — setup"
echo "==============================================="

# 1. build tools
if ! command -v cc >/dev/null 2>&1; then
    echo "[1/3] Installing build tools (needs sudo)..."
    sudo apt update && sudo apt install -y build-essential pkg-config curl
else
    echo "[1/3] Build tools already present."
fi

# 2. Rust toolchain
if ! command -v cargo >/dev/null 2>&1; then
    echo "[2/3] Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
echo "[2/3] Rust: $(cargo --version)"

# 3. build the optimised binary
echo "[3/3] Building the bot (this takes a couple of minutes the first time)..."
cargo build --release

# create .env from the template if it doesn't exist yet
if [ ! -f .env ]; then
    cp .env.example .env
    echo ""
    echo ">>> Created .env — NOW EDIT IT AND ADD YOUR API KEYS:"
    echo ">>>     nano .env"
fi

echo ""
echo "==============================================="
echo " Setup complete."
echo " Next: 1) edit your keys ->  nano .env"
echo "       2) start the bot  ->  ./run.sh"
echo "==============================================="
