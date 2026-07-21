#!/usr/bin/env bash
#
# Run this after replacing the bot files with a new version.
#
#   ./update.sh
#
# The zip only contains SOURCE code. The bot that actually runs is a compiled
# binary in target/release/. Unzipping new files WITHOUT rebuilding leaves the
# old binary in place and nothing changes — this script does the rebuild and
# the restart for you, then shows you which version ended up running.
set -e
cd "$(dirname "$0")"

echo ">>> 1/4  Building the new version (this takes 1-2 minutes)..."
source "$HOME/.cargo/env" 2>/dev/null || true
cargo build --release

echo
echo ">>> 2/4  Checking your settings are intact..."
if [ ! -f .env ]; then
    echo "!! No .env found. Your keys and settings are missing."
    echo "!! Run ./setup.sh first, then edit .env."
    exit 1
fi
grep -E "^(SYMBOL|LEVERAGE|STOP_LOSS_PERCENTAGE|TAKE_PROFIT_PERCENTAGE|USE_DEMO|TESTNET)=" .env

echo
echo ">>> 3/4  Restarting the bot..."
if systemctl list-unit-files 2>/dev/null | grep -q '^hf-trading-bot.service'; then
    sudo systemctl restart hf-trading-bot
    sleep 3
    echo "Service restarted."
else
    echo "No systemd service installed — start the bot with ./run.sh"
fi

echo
echo ">>> 4/4  Version now running:"
if systemctl list-unit-files 2>/dev/null | grep -q '^hf-trading-bot.service'; then
    sudo journalctl -u hf-trading-bot -n 30 --no-pager | grep -E "Bot version|Trading mode" || \
        echo "(no version line yet — check with: sudo journalctl -u hf-trading-bot -f)"
else
    ./target/release/hf_trading_bot --version 2>/dev/null || true
fi

echo
echo "Done. Watch it live with:   sudo journalctl -u hf-trading-bot -f"
echo "The stop-loss line will read:  Placed stop loss at ... = X% from entry ..."
