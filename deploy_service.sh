#!/usr/bin/env bash
#
# Install the bot as a 24/7 auto-restarting service (systemd) on Ubuntu.
# Auto-detects the current folder + user, so it works wherever you put it.
# Runs in the DEFAULT watch/confirm entry mode (option 2).
#
# Usage:  ./deploy_service.sh
#
set -e
cd "$(dirname "$0")"

DIR="$(pwd)"
USER_NAME="$(whoami)"
BIN="$DIR/target/release/hf_trading_bot"

if [ ! -x "$BIN" ]; then
    echo "The bot isn't built yet. Run ./setup.sh first."
    exit 1
fi
if [ ! -f "$DIR/.env" ] || grep -qE "paste_your_|your_api_" "$DIR/.env"; then
    echo "Set your real keys in .env before deploying:  nano .env"
    exit 1
fi

echo "Installing systemd service (needs sudo)..."
sudo tee /etc/systemd/system/hf-trading-bot.service >/dev/null <<EOF
[Unit]
Description=Binance HF Futures Trading Bot (Rust)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$USER_NAME
WorkingDirectory=$DIR
# Entry mode comes from .env (ENTRY_MODE=auto|custom) — no interactive prompt.
ExecStart=$BIN
# on-failure (NOT always): after take-profit the bot exits cleanly (code 0) and
# must STAY stopped until the user starts it again. Only restart on a crash.
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now hf-trading-bot

echo ""
echo "Service installed and started."
echo "  Live logs : journalctl -u hf-trading-bot -f"
echo "  Status    : sudo systemctl status hf-trading-bot"
echo "  Stop      : sudo systemctl stop hf-trading-bot"
echo "  Restart   : sudo systemctl restart hf-trading-bot   (after editing .env)"
