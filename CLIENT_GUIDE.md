# Your Trading Bot — Complete Guide

This is your Binance futures bot, rebuilt in **Rust** for speed. It behaves
exactly like your old Python bot (same entry logic, market stop-loss + flip,
limit take-profit, stops after take-profit) — but the delays that caused
slippage are gone.

**You only need to do two things:** (1) put your API keys in one file, and
(2) run one command. This guide walks you through everything.

---

## What you'll need

- Your Ubuntu server (AWS Tokyo) — or any Ubuntu machine for testing.
- **API keys:**
  - For testing → **demo** keys from <https://demo.binance.com> (fake money).
  - For real trading → your **live** Binance Futures API keys.

> Your keys are read from a simple text file (`.env`). They are **not** built
> into the program, so you can change them anytime — just edit the file and
> restart. No developer needed to rotate keys.

---

## STEP 1 — Put the files on the server

Copy the `hf-bot-rust` folder to your server (from your PC):

```bash
scp -r hf-bot-rust ubuntu@YOUR_SERVER_IP:/home/ubuntu/
```

Then connect to the server:

```bash
ssh ubuntu@YOUR_SERVER_IP
cd hf-bot-rust
```

---

## STEP 2 — One-time setup (installs & builds everything)

```bash
./setup.sh
```

That's it — it installs Rust, builds the bot, and creates your `.env` file.
It takes a couple of minutes the first time. You only ever run this once.

---

## STEP 3 — Enter your API keys (and settings)

Open the settings file:

```bash
nano .env
```

You'll see this. **Edit the lines and put your own values:**

```
BINANCE_API_KEY=paste_your_key_here
BINANCE_API_SECRET=paste_your_secret_here

USE_DEMO=true            # true = demo/testing, false = real money
API_TIMEOUT=30

SYMBOL=XRPUSDT           # the coin/pair to trade
LEVERAGE=15              # leverage
POSITION_SIZE=10         # position size in USDT
TAKE_PROFIT_PERCENTAGE=2.0
STOP_LOSS_PERCENTAGE=0.1 # your tight stop, e.g. 0.1%
WATCH_PERCENTAGE=0.05
CONFIRM_PERCENTAGE=0.10
```

**This is the ONLY file you edit** — coin, leverage, stop-loss, take-profit,
and your keys all live here.

To save in nano: press `Ctrl+O`, then `Enter`, then `Ctrl+X`.

> Keep `USE_DEMO=true` while testing. Change it to `false` only when you're
> ready to trade real money.

---

## STEP 4 — Run the bot

```bash
./run.sh
```

It will ask how you want to enter trades:

```
Choose entry mode:
  1) Custom entry price range
  2) Default percentage (watch/confirm)
```

- Type **2** for the normal automatic mode (same as your old bot's default).
- Type **1** to set a LOW and HIGH price yourself (bot goes SHORT if LOW is
  hit, LONG if HIGH is hit).

To stop the bot: press **Ctrl+C**. It safely cancels your orders and closes any
open position before exiting.

---

## STEP 5 — Test it on demo first (recommended)

With `USE_DEMO=true` and demo keys, run the bot and confirm it does all of this
(watch the messages on screen, and check the Positions/Orders on Binance):

- ✅ Connects and sets your leverage.
- ✅ Opens a position (you'll see the take-profit and stop-loss get placed).
- ✅ When the stop-loss is hit, it **instantly flips** to the opposite side.
- ✅ When take-profit is hit, it **cancels everything and stops** (won't reopen
  until you run it again).
- ✅ If you manually change the TP/SL in the Binance app, the bot leaves your
  change alone (it only re-manages after it's hit).

**Tip:** to make trades happen quickly while testing, temporarily use small
numbers, e.g. `TAKE_PROFIT_PERCENTAGE=0.5`, `STOP_LOSS_PERCENTAGE=0.3`,
`WATCH_PERCENTAGE=0.03`, `CONFIRM_PERCENTAGE=0.05`. Put your real values back
afterwards.

---

## STEP 6 — Go live

1. Edit `.env`: set `USE_DEMO=false` and paste your **live** keys.
2. Put back the real `SYMBOL`, `LEVERAGE`, `TAKE_PROFIT_PERCENTAGE`,
   `STOP_LOSS_PERCENTAGE` you want.
3. Start it — either by hand (`./run.sh`) or as a 24/7 service (next step).

---

## STEP 7 — Run 24/7 with auto-restart (for live trading)

This keeps the bot running after you log out, and restarts it automatically if
it ever stops or the server reboots. It runs in the default (option 2) mode.

```bash
./deploy_service.sh
```

Everyday commands:

```bash
journalctl -u hf-trading-bot -f          # watch what the bot is doing (live)
sudo systemctl restart hf-trading-bot    # apply changes after editing .env
sudo systemctl stop hf-trading-bot       # stop the bot
sudo systemctl status hf-trading-bot     # is it running?
```

> If you want to use the **custom price range** (option 1) instead, run it by
> hand with `./run.sh` inside a `tmux` session rather than as the 24/7 service
> (the service uses the automatic mode because a range needs you to type prices).

---

## Keeping it running when you close your terminal (manual mode)

If you run with `./run.sh` and want it to keep going after you disconnect:

```bash
sudo apt install -y tmux
tmux new -s bot          # start a session
./run.sh                 # run the bot
# press Ctrl+B then D to leave it running in the background
tmux attach -t bot       # come back to it later
```

---

## Checking that the stop loss is exactly what you set

Every time the bot opens a position it now prints the whole stop-loss
calculation, then reads the order back from Binance and prints what the
**exchange** is actually holding. You don't have to take the bot's word for it.

You'll see three lines like this (here `STOP_LOSS_PERCENTAGE=0.1`):

```
Opened long position at 0.15989 [order avgPrice] (signal price 0.15992, ordered 62, filled 62)
SL calc | side=LONG | entry=0.15989 (from order avgPrice) | requested=0.1% | raw=0.15973011 | tick=0.00001 | rounded=0.15973 | actual=0.1001% | qty=62 | leverage=1x (not used in this maths)
VERIFIED on Binance: stop 0.15973 is 0.1001% from entry 0.15989 (configured 0.1%, tolerance 0.0063% = one tick)
```

What to look at:

| Field | What it means |
|---|---|
| `entry=` and `(from ...)` | The price your position really filled at, and where that number came from. `order avgPrice` is the best source. |
| `requested=` | The `STOP_LOSS_PERCENTAGE` from your `.env`. |
| `actual=` | What the stop actually works out to. This must match `requested`. |
| `VERIFIED on Binance` | The bot re-read the order from the exchange. This is the number you see in the Binance app. |
| `Stop quantity verified` | The stop covers your **whole** position, so nothing is left open if it fires. |

If anything is ever wrong you will see `STOP MISMATCH on Binance` or
`SL SANITY CHECK FAILED` in **red**. Send me that line and I can fix it from
that alone — it contains every number that went into the decision.

**Why it was wrong before:** the stop was being measured from Binance's *mark
price* rather than the price your order actually filled at. Mark price is a
reference number Binance calculates for funding and liquidation; on a thin coin
it drifts away from the price trades actually happen at. On `SOLUSDT` that gap
is essentially zero, which is why your SOL stops looked right. On `BANKUSDT` it
is routinely 0.04% and goes far higher when the price moves — so a 0.1% stop
was already off by more than half its own size before the percentage was even
applied, and it was off by a *different* amount on every trade. The bot now
follows the real traded price, and measures the stop from the actual fill.

You can see that gap for yourself on any coin:

```bash
cargo run --release --example stream_check -- BANKUSDT
```

**One thing to know:** a stop can only ever be placed on the coin's price grid
(its "tick size"). On `BANKUSDT` one tick is `0.00001`, which at a price of
0.16 is about 0.006% — so any stop you set is accurate to within that. If you
set a percentage *smaller* than one tick, the bot places the closest legal stop
and tells you so in the log rather than silently doing something else.

---

## Troubleshooting

| Message / problem | What to do |
|---|---|
| `Missing in .env: BINANCE_API_KEY...` | You haven't added your keys. Run `nano .env` and paste them. |
| `.env still has PLACEHOLDER keys` | Same — edit `.env` with your real keys. |
| API error about permissions / `-2015` | Your API key needs **Futures** enabled; check it on Binance. |
| Nothing trades | It's waiting for the price to move enough to trigger. Lower the watch/confirm % to test, or use the custom range (option 1). |
| Want more detail in the logs | Run with `RUST_LOG=debug ./run.sh` |
| `STOP MISMATCH on Binance` / `SL SANITY CHECK FAILED` / `STOP QUANTITY MISMATCH` | Send me that line **and the `Raw response…` line under it** — together they contain everything needed to fix it. |
| `account is in HEDGE mode` | Binance app → Futures → Preferences → Position Mode → **One-way**, then restart the bot. |
| `Holding the ... level as a bot-side stop` | Binance refused the stop order, so the bot is enforcing it itself at the same price. Still protected, but only while the bot is running — don't stop it. |
| Stop is a fraction of a % off | That's the coin's tick size (its price grid); the log line shows the tolerance. It can't be finer than one tick. |

---

## Quick reference

| I want to… | Command |
|---|---|
| Set up the bot (once) | `./setup.sh` |
| Change keys / coin / SL / TP | `nano .env` |
| Start the bot | `./run.sh` |
| Run 24/7 with auto-restart | `./deploy_service.sh` |
| Watch live logs (24/7 mode) | `journalctl -u hf-trading-bot -f` |
| Stop the bot | `Ctrl+C` (manual) or `sudo systemctl stop hf-trading-bot` |

Any issues, send me the messages you see on screen and I'll sort it out.
