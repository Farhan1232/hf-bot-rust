#!/usr/bin/env bash
#
# Start the bot.   Usage:  ./run.sh
#
set -e
cd "$(dirname "$0")"

BIN="./target/release/hf_trading_bot"

if [ ! -x "$BIN" ]; then
    echo "The bot isn't built yet. Run ./setup.sh first."
    exit 1
fi

if [ ! -f .env ]; then
    echo "No .env file found. Run ./setup.sh, then edit .env with your keys."
    exit 1
fi

if grep -qE "paste_your_demo_key_here|paste_your_demo_secret_here|your_api_key_here|your_api_secret_here" .env; then
    echo "!! Your .env still has PLACEHOLDER keys."
    echo "!! Edit it and paste your real keys:   nano .env"
    exit 1
fi

exec "$BIN"
