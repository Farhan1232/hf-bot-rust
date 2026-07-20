# Testing with FREE keys — no ID / no KYC

You do **not** need to give Binance your passport/ID to test this bot. Binance
runs a separate **Futures Testnet** with fake money and free API keys. This is
the standard way developers test trading bots.

## Get free testnet API keys (2 minutes)

1. Go to **https://testnet.binancefuture.com**
2. Click **Log In** (top right) and sign in with **GitHub** or **Google**.
   - No ID, no phone, no KYC. Just the GitHub/Google login.
3. You automatically get a large fake USDT balance to trade with.
4. Scroll to the **bottom of the page** — there's an **API Key** panel.
   Click **Generate** / **Create**.
5. Copy the **API Key** and the **API Secret** (secret is shown once).

> These testnet keys ONLY work on the testnet. They are separate from demo
> keys and from live keys.

## Put them in the bot

The `.env` file is already set to testnet mode for you:

```bash
nano .env
```

Set these two lines:

```
BINANCE_API_KEY=your_testnet_key
BINANCE_API_SECRET=your_testnet_secret
```

and make sure these are set (they already are in the shipped `.env`):

```
TESTNET=true
USE_DEMO=false
```

Save: `Ctrl+O`, Enter, `Ctrl+X`.

## Run it

```bash
./run.sh
```

Pick `2` (automatic) or `1` (type a price range). You should see:

```
Trading mode: TESTNET
Leverage set to 15x
Margin type set to CROSSED
Tracking price: 1.10...
```

Then it will trade with the fake balance. Watch it open a position, place the
take-profit and stop-loss, flip on a stop-loss hit, and stop after take-profit.
Nothing here touches real money.

## Switching back for the client / real trading

- **Demo** (client's original environment): `TESTNET=false`, `USE_DEMO=true`,
  demo keys from https://demo.binance.com
- **Live** (real money): `TESTNET=false`, `USE_DEMO=false`, live Binance keys

That's the only change — the bot behaves identically in all three.
