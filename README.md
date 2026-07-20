# Binance HF Futures Trading Bot — Rust

A Rust port of the original Python bot. **Same trading logic**, rebuilt for
speed: the per-order Binance time re-sync and all the `sleep()` pauses in the
open/close/flip path are gone, so a stop-loss flip happens in one network
round-trip per action instead of the previous 4–5 seconds.

## Behaviour (unchanged from the Python version)

- Two-stage **watch → confirm** entry, or an optional **custom entry price range**.
- Opens with a **MARKET** order.
- **Take profit** = reduce-only **LIMIT** order.
- **Stop loss** = reduce-only **STOP_MARKET** order (retries on Binance error
  `-2021`, and emergency-closes at market if it truly cannot be placed).
- On **stop-loss fill → flips** to the opposite side with a MARKET order.
- On **take-profit fill → cancels all orders and STOPS** (restart to resume).
- You can edit TP/SL from the Binance app while a position is open; the bot
  only re-manages orders after they are hit (it won't stomp a manual edit).

## 1. Build

Install Rust (once):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Build the optimised binary:

```bash
cargo build --release
# binary: target/release/hf_trading_bot
```

## 2. Configure

```bash
cp .env.example .env
nano .env   # set your keys + coin / leverage / SL / TP
```

`.env` is where you set **coin (SYMBOL), leverage, position size, take profit %,
stop loss %**, and whether you are on demo/testnet (`USE_DEMO=true`) or live
(`USE_DEMO=false`). Keys are read at runtime — rotate them anytime by editing
`.env`, no rebuild needed.

## 3. Run

```bash
./target/release/hf_trading_bot
```

You'll be asked to pick the entry mode:

- `1` = custom entry price range (you type a LOW and HIGH price)
- `2` = default percentage watch/confirm mode

To turn logging up: `RUST_LOG=debug ./target/release/hf_trading_bot`

## 4. Deploy on Ubuntu (AWS Tokyo) — 24/7 with auto-restart

See **RUN_GUIDE.md** for the full step-by-step. Short version:

```bash
sudo cp deploy/hf-trading-bot.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now hf-trading-bot
journalctl -u hf-trading-bot -f      # live logs
```

## Notes

- Uses `rustls` for TLS, so there are no OpenSSL system dependencies to install
  on a fresh Ubuntu server.
- The mark-price stream uses `@markPrice` (same as the Python bot). If you want
  faster price ticks for entry detection, change it to `@markPrice@1s` in
  `src/websocket.rs` — the flip/slippage path is driven by order-fill events and
  is unaffected either way.
- Demo/live hosts match the Python bot: `https://demo-fapi.binance.com` /
  `wss://demo-fstream.binance.com` for demo, and the standard
  `fapi.binance.com` / `fstream.binance.com` for live.
