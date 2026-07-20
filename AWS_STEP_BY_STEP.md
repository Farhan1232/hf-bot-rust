# AWS Tokyo Ubuntu — create the server + deploy (beginner steps)

You do NOT create an AWS account — Waleed already has one. You only create the
SERVER (an "EC2 instance") inside his account, then put the bot on it.

===================================================================
PART 1 — Open EC2 and set the region to Tokyo
===================================================================
1. In Waleed's AWS page, in the top search bar type: EC2  → click "EC2".
2. TOP-RIGHT corner: click the region name and choose
   "Asia Pacific (Tokyo)  ap-northeast-1".
   (Very important — the server must be in Tokyo for speed.)

===================================================================
PART 2 — Launch the Ubuntu server
===================================================================
3. Click the orange button "Launch instance".
4. Name:  hf-trading-bot
5. "Application and OS Images":
      - Click "Ubuntu"
      - Choose "Ubuntu Server 24.04 LTS" (64-bit x86, says Free tier eligible)
6. "Instance type":  choose  t3.small
      (if unavailable, t3.medium is fine. Do NOT pick t2.micro — too weak.)
7. "Key pair (login)":
      - Click "Create new key pair"
      - Name: hf-bot-key
      - Type: RSA     Format: .pem
      - Click "Create key pair"
      - A file  hf-bot-key.pem  DOWNLOADS to the PC (Downloads folder).
        >>> REMEMBER where it is. Don't lose it. <<<
8. "Network settings":
      - Make sure "Allow SSH traffic from" is ticked (Anywhere is OK).
9. "Configure storage": change 8 GiB  ->  20 GiB.
10. Click "Launch instance" (bottom right).
11. Click "View all instances". Wait ~1 minute until state = 🟢 Running.
12. Click the instance and COPY the "Public IPv4 address" (e.g. 13.112.xx.xx).

===================================================================
PART 3 — Connect to the server (EASIEST way, no key needed)
===================================================================
13. In the instances list, TICK your instance.
14. Click the "Connect" button (top).
15. Choose the "EC2 Instance Connect" tab  ->  click "Connect".
16. A BLACK TERMINAL opens in the browser. This is your Ubuntu server. ✅

===================================================================
PART 4 — Put the bot on the server and run it
===================================================================
(Do this in that black browser terminal. Ask Farhan/Claude for the file-copy
command — it depends on how the zip gets there. Once the folder is on the
server:)
   cd hf-bot-rust
   ./setup.sh                 # installs Rust + builds (a few minutes)
   nano .env                  # paste LIVE keys, set TESTNET=false USE_DEMO=false
   ./deploy_service.sh        # run 24/7 with auto-restart
   journalctl -u hf-trading-bot -f     # watch it live

===================================================================
Manage it later
===================================================================
   sudo systemctl restart hf-trading-bot   # after editing .env
   sudo systemctl stop hf-trading-bot      # stop
   sudo systemctl status hf-trading-bot    # check status
