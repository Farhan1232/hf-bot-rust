# Run Guide — Binance HF Trading Bot (Rust) on Ubuntu / AWS Tokyo

This is the plain, step-by-step guide. (Open it in any Markdown viewer, or
"Print → Save as PDF" to get a PDF copy.)

---

## A. One-time setup on the Ubuntu server

1. **Connect to your AWS Tokyo server**

   ```bash
   ssh ubuntu@YOUR_SERVER_IP
   ```

2. **Install Rust + build tools** (only needed once)

   ```bash
   sudo apt update && sudo apt install -y build-essential pkg-config
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   source "$HOME/.cargo/env"
   ```

3. **Copy the project to the server** (from your PC), e.g.

   ```bash
   scp -r hf-bot-rust ubuntu@YOUR_SERVER_IP:/home/ubuntu/
   ```

4. **Build the bot**

   ```bash
   cd /home/ubuntu/hf-bot-rust
   cargo build --release
   ```

   The finished program is at `target/release/hf_trading_bot`.

---

## B. Set your trading settings (coin, leverage, SL, TP)

All settings live in one file: **`.env`**.

```bash
cd /home/ubuntu/hf-bot-rust
cp .env.example .env
nano .env
```

Edit these lines:

| Setting | What it does | Example |
|---|---|---|
| `BINANCE_API_KEY` | Your Binance API key | `abcd...` |
| `BINANCE_API_SECRET` | Your Binance API secret | `wxyz...` |
| `USE_DEMO` | `true` = testnet (no real money), `false` = live | `true` |
| `SYMBOL` | The coin/pair to trade | `XRPUSDT` |
| `LEVERAGE` | Leverage | `15` |
| `POSITION_SIZE` | Position size in USDT | `10` |
| `TAKE_PROFIT_PERCENTAGE` | Take-profit % | `2.0` |
| `STOP_LOSS_PERCENTAGE` | Stop-loss % (your tight stop, e.g. 0.1) | `0.1` |
| `WATCH_PERCENTAGE` | Stage-1 watch threshold % | `0.05` |
| `CONFIRM_PERCENTAGE` | Stage-2 confirm threshold % | `0.10` |

Save in nano: `Ctrl+O`, `Enter`, then `Ctrl+X`.

> You can change these anytime and just restart the bot. No rebuild needed.
> To rotate your keys, edit `.env` — nothing is hardcoded in the program.

---

## C. Run the bot by hand (like `python main` before)

```bash
cd /home/ubuntu/hf-bot-rust
./target/release/hf_trading_bot
```

It will ask:

```
Choose entry mode:
  1) Custom entry price range
  2) Default percentage (watch/confirm)
```

- Type **2** for the normal percentage mode.
- Type **1** to enter a LOW and HIGH price (bot goes SHORT if LOW is hit,
  LONG if HIGH is hit).

Stop it anytime with **Ctrl+C** — it cancels open orders and closes any open
position before exiting.

**Tip — keep it running after you log out** (using `tmux`):

```bash
sudo apt install -y tmux
tmux new -s bot
./target/release/hf_trading_bot
# detach and leave it running: press Ctrl+B then D
# come back later:
tmux attach -t bot
```

---

## D. Run 24/7 with auto-restart (recommended for live)

This uses `systemd` so the bot restarts automatically if it ever stops, and
starts on server reboot. It runs in **default watch/confirm mode** (option 2).

```bash
cd /home/ubuntu/hf-bot-rust
sudo cp deploy/hf-trading-bot.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now hf-trading-bot
```

Common commands:

```bash
journalctl -u hf-trading-bot -f      # watch live logs
sudo systemctl stop hf-trading-bot   # stop
sudo systemctl restart hf-trading-bot# restart (e.g. after editing .env)
sudo systemctl status hf-trading-bot # check state
```

> The custom entry price range needs you to type numbers, so use section C
> (tmux) for that mode. The 24/7 service is for the default mode.

---

## E. Optional: pin the bot to one CPU core (lowest jitter)

The service file has a commented `taskset` line. To dedicate CPU core #1 to the
bot, edit `/etc/systemd/system/hf-trading-bot.service`, comment the plain
`ExecStart`, uncomment the `taskset` one, then:

```bash
sudo systemctl daemon-reload
sudo systemctl restart hf-trading-bot
```

---

## F. Quick testnet check before going live

1. In `.env` set `USE_DEMO=true` and use your **demo** keys from
   <https://demo.binance.com>.
2. Run the bot (section C). Open/close a position and confirm the flip on
   stop-loss and the stop-after-take-profit both behave as expected.
3. When happy, set `USE_DEMO=false`, put in your live keys, and restart.
