# Deploy on AWS Ubuntu (Tokyo) — step by step

This runs the bot 24/7 on an AWS Ubuntu server in Tokyo (next to Binance =
fastest execution). All commands are copy-paste.

---

## What you need first

1. The server's **public IP address** (e.g. `13.112.45.xx`)
2. A way to log in — either an AWS **`.pem` key file**, or a **password**
3. The login **username** — for Ubuntu on AWS it's usually `ubuntu`
4. Your **live Binance API keys** — Futures-only, withdrawals OFF

Server should be: **Region = Tokyo (ap-northeast-1)**, **OS = Ubuntu Server 22.04/24.04**.

---

## Step 1 — Connect (from your PC terminal)

With a `.pem` key:
```bash
chmod 400 ~/Downloads/key.pem
ssh -i ~/Downloads/key.pem ubuntu@SERVER_IP
```
With a password:
```bash
ssh ubuntu@SERVER_IP
```
(First time it asks to confirm — type `yes`.)

## Step 2 — Copy the bot up (in a SECOND terminal on your PC)

With a `.pem` key:
```bash
scp -i ~/Downloads/key.pem ~/hf-bot-rust.zip ubuntu@SERVER_IP:~
```
With a password:
```bash
scp ~/hf-bot-rust.zip ubuntu@SERVER_IP:~
```
Then, back in the server terminal:
```bash
sudo apt update && sudo apt install -y unzip
unzip hf-bot-rust.zip
cd hf-bot-rust
```

## Step 3 — Build (one time)
```bash
./setup.sh
```

## Step 4 — Test on testnet first (2 minutes, recommended)
```bash
nano .env      # TESTNET=true , paste testnet keys
./run.sh       # type 1 then LOW 1.00 / HIGH 1.08, or type 2
```
Check the `⏱ Position opened + protected in ...` line — on Tokyo it should be a
fraction of a second. Press `Ctrl+C` to stop.

## Step 5 — Switch to live
```bash
nano .env
```
Set:
```
TESTNET=false
USE_DEMO=false
BINANCE_API_KEY=<your live key>
BINANCE_API_SECRET=<your live secret>
```
Also set the real SYMBOL / LEVERAGE / STOP_LOSS_PERCENTAGE / TAKE_PROFIT_PERCENTAGE. Save.

## Step 6 — Run 24/7 with auto-restart
```bash
./deploy_service.sh
journalctl -u hf-trading-bot -f      # watch it live
```
Manage it:
```bash
sudo systemctl restart hf-trading-bot   # after editing .env
sudo systemctl stop hf-trading-bot      # stop
sudo systemctl status hf-trading-bot    # check status
```

## Step 7 — Disconnect / reconnect
- Leave (bot keeps running): type `exit`
- Come back: `ssh` in again (Step 1)

---

### Safety
- Use **Futures-only, no-withdrawal** API keys.
- Always do the testnet check (Step 4) on the server before going live.
- To change coin / leverage / SL / TP later: `nano .env`, then
  `sudo systemctl restart hf-trading-bot`.
