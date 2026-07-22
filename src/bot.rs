//! The trading engine — a faithful port of `trading_bot.py`.
//!
//! SAME LOGIC as the Python bot:
//!   * two-stage watch -> confirm entry (or a custom entry price range),
//!   * open with a MARKET order,
//!   * TAKE PROFIT as a reduce-only LIMIT order,
//!   * STOP LOSS as a reduce-only STOP_MARKET order (with -2021 retry +
//!     emergency market close if it cannot be placed),
//!   * on stop-loss fill -> FLIP to the opposite side with a MARKET order,
//!   * on take-profit fill -> cancel everything and STOP the bot,
//!   * a consistency check reconciles tracked vs. real position and clears
//!     orphan orders, but leaves a user's own TP/SL edits alone (order count
//!     of 2 is left untouched — client requirement #8).
//!
//! WHAT CHANGED FOR SPEED (client requirements #2 and #3):
//!   * every `time.sleep(...)` in the open/close/flip hot path is GONE,
//!   * no per-order Binance time re-sync round-trip,
//! so a close-and-flip now happens in one network round-trip per action
//! instead of the 4–5 seconds that turned a 0.1% stop into ~1% slippage.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use tracing::{error, info, warn};

use crate::config::TradingConfig;
use crate::events::{Event, OrderUpdate};
use crate::rest::{get_f64, get_i64, ApiError, BinanceRest, SymbolFilters};


pub struct TradingBot {
    config: TradingConfig,
    rest: Arc<BinanceRest>,
    filters: SymbolFilters,

    // --- entry state ---
    entry_price: Option<f64>,    // reference price for watch/confirm
    watch_direction: Option<String>,
    watch_price: Option<f64>,
    use_custom_entry: bool,

    // --- position state ---
    current_position: Option<String>, // "long" / "short"
    position_entry_price: Option<f64>,
    /// algoId of the exchange-side stop-loss (CONDITIONAL algo order). Stored so
    /// we can cancel it on flip/TP/shutdown. `None` if no algo stop is active.
    stop_algo_id: Option<i64>,
    /// Set only as a LAST RESORT if the exchange stop can't be placed at all.
    /// The bot then monitors price itself and market-closes when this level is
    /// breached. `None` means the stop is a real exchange order.
    software_stop_price: Option<f64>,

    // --- stats ---
    initial_balance: f64,
    current_balance: f64,
    trades_count: u64,
    profitable_trades: u64,

    last_price: Option<f64>,
    last_logged_price: Option<f64>,

    pub stop_flag: bool,
}

impl TradingBot {
    pub fn new(
        config: TradingConfig,
        rest: Arc<BinanceRest>,
        filters: SymbolFilters,
        initial_balance: f64,
    ) -> Self {
        let use_custom_entry = config.custom_entry_price_low.is_some();
        if use_custom_entry {
            info!(
                "Custom entry range armed: LOW={:?} (-> SHORT), HIGH={:?} (-> LONG). \
                 The bot enters ONLY at these prices (percentage/auto entry is not used). \
                 Stop loss uses {}%.",
                config.custom_entry_price_low,
                config.custom_entry_price_high,
                config.stop_loss_percentage
            );
        }
        TradingBot {
            config,
            rest,
            filters,
            entry_price: None,
            watch_direction: None,
            watch_price: None,
            use_custom_entry,
            current_position: None,
            position_entry_price: None,
            stop_algo_id: None,
            software_stop_price: None,
            initial_balance,
            current_balance: initial_balance,
            trades_count: 0,
            profitable_trades: 0,
            last_price: None,
            last_logged_price: None,
            stop_flag: false,
        }
    }

    pub fn log_startup(&self) {
        let entry_mode = if self.use_custom_entry {
            format!(
                "Custom Entry Range: LOW={:?} (-> SHORT), HIGH={:?} (-> LONG), watch/confirm bypassed",
                self.config.custom_entry_price_low, self.config.custom_entry_price_high
            )
        } else {
            format!(
                "Watch Threshold: {}% / Confirm Threshold: {}%",
                self.config.watch_percentage, self.config.confirm_percentage
            )
        };
        let mode = self.config.env_name();
        info!(
            "\n=== Bot Starting ===\nMode: {mode}\nSymbol: {}\nInitial Balance: {:.4} USDT\n\
             Leverage: {}x\nPosition Size: {} USDT\nTake Profit: {}%\n{entry_mode}\nStop Loss: {}%\n=================",
            self.config.symbol,
            self.initial_balance,
            self.config.leverage,
            self.config.position_size,
            self.config.take_profit_percentage,
            self.config.stop_loss_percentage
        );
    }

    // -----------------------------------------------------------------
    // Event dispatch (called by the single-owner loop in main)
    // -----------------------------------------------------------------
    pub async fn handle(&mut self, event: Event) {
        match event {
            Event::Price(p) => self.on_price(p).await,
            Event::OrderFill(o) => self.on_order_fill(o).await,
            Event::ConsistencyTick => self.consistency_check().await,
            Event::Heartbeat => self.heartbeat(),
        }
    }

    // -----------------------------------------------------------------
    // Price handling: two-stage watch/confirm or custom entry range
    // -----------------------------------------------------------------
    async fn on_price(&mut self, price: f64) {
        self.last_price = Some(price);

        // Log notable moves (>0.5%) like the Python bot.
        match self.last_logged_price {
            None => {
                self.last_logged_price = Some(price);
                info!("Tracking price: {price}");
            }
            Some(lp) => {
                if (price / lp - 1.0).abs() > 0.005 {
                    info!(
                        "Price moved: {lp} -> {price} ({:+.2}%)",
                        (price / lp - 1.0) * 100.0
                    );
                    self.last_logged_price = Some(price);
                }
            }
        }

        if self.entry_price.is_none() {
            self.entry_price = Some(price);
            info!("Initial reference price set to {price}");
            if !self.use_custom_entry {
                return;
            }
            // custom entry: fall through so an already-out-of-range first tick
            // triggers immediately.
        }

        // While in a position: run the bot-side software stop (only armed when
        // the venue rejected a resting STOP_MARKET), then stop — no new entries.
        if self.current_position.is_some() {
            if let Some(sl) = self.software_stop_price {
                let breached = match self.current_position.as_deref() {
                    Some("long") => price <= sl,
                    Some("short") => price >= sl,
                    _ => false,
                };
                if breached {
                    self.fire_software_stop(price).await;
                }
            }
            return;
        }

        // Custom entry mode: ALWAYS enter by the custom price range, and NEVER
        // fall through to the percentage (auto) logic. This guarantees the bot
        // only ever opens when the price actually reaches the LOW/HIGH the user
        // set — it re-arms after every close (fixes: bot opening on a small %
        // move instead of the custom price after the first trade).
        if self.use_custom_entry {
            let low = self.config.custom_entry_price_low.unwrap();
            let high = self.config.custom_entry_price_high.unwrap();
            if price >= high {
                info!("Custom entry HIGH reached: {price} (target {high}). Opening LONG.");
                self.open_position("long", price, false).await;
            } else if price <= low {
                info!("Custom entry LOW reached: {price} (target {low}). Opening SHORT.");
                self.open_position("short", price, false).await;
            }
            return;
        }

        match self.watch_direction.clone() {
            None => {
                // Stage 1: first threshold crossing starts a watch.
                let change = self.price_change_pct(price);
                if change >= self.config.watch_percentage {
                    self.watch_direction = Some("long".into());
                    self.watch_price = Some(price);
                    info!(
                        "Watching LONG setup: {change:+.2}% to {price} (ref {:?}). Need {}% to confirm.",
                        self.entry_price, self.config.confirm_percentage
                    );
                } else if change <= -self.config.watch_percentage {
                    self.watch_direction = Some("short".into());
                    self.watch_price = Some(price);
                    info!(
                        "Watching SHORT setup: {change:+.2}% to {price} (ref {:?}). Need {}% to confirm.",
                        self.entry_price, self.config.confirm_percentage
                    );
                }
            }
            Some(dir) => {
                // Stage 2: confirm or invalidate.
                let change = self.price_change_pct(price);
                let confirmed = (dir == "long" && change >= self.config.confirm_percentage)
                    || (dir == "short" && change <= -self.config.confirm_percentage);

                if confirmed {
                    info!("{} entry confirmed at {price} (ref {:?})", dir.to_uppercase(), self.entry_price);
                    self.watch_direction = None;
                    self.watch_price = None;
                    self.open_position(&dir, price, false).await;
                } else {
                    // If price is no longer in the watched zone/direction, re-anchor.
                    let cur = if change >= self.config.watch_percentage {
                        Some("long")
                    } else if change <= -self.config.watch_percentage {
                        Some("short")
                    } else {
                        None
                    };
                    if cur != Some(dir.as_str()) {
                        info!("{} setup invalidated — price pulled back to {price}. Re-arming.", dir.to_uppercase());
                        self.watch_direction = None;
                        self.watch_price = None;
                        self.entry_price = Some(price);
                    }
                }
            }
        }
    }

    fn price_change_pct(&self, price: f64) -> f64 {
        match self.entry_price {
            Some(e) if e > 0.0 => (price / e - 1.0) * 100.0,
            _ => 0.0,
        }
    }

    // -----------------------------------------------------------------
    // Opening a position (MARKET) + placing TP (LIMIT) and SL (STOP_MARKET)
    // No sleeps: this is the hot path the speed fix targets.
    // -----------------------------------------------------------------
    /// Fire the bot-side software stop: market-close the position. The
    /// resulting reduce-only MARKET fill arrives on the user stream and is
    /// detected exactly like a stop-loss hit, so the normal flip logic runs.
    async fn fire_software_stop(&mut self, price: f64) {
        let level = self.software_stop_price;
        info!(
            "Software stop-loss hit at {price} (level {:?}) — closing {:?} at market",
            level, self.current_position
        );
        // Disarm so we don't fire again before the close fill arrives.
        self.software_stop_price = None;

        match self.rest.position_information(&self.config.symbol).await {
            Ok(positions) => {
                for p in &positions {
                    let amt = get_f64(p, "positionAmt");
                    if amt != 0.0 {
                        let side = if amt > 0.0 { "SELL" } else { "BUY" };
                        let qty = self.format_quantity(amt.abs());
                        if let Err(e) = self
                            .rest
                            .create_market_order(&self.config.symbol, side, &qty, true)
                            .await
                        {
                            error!("Software stop close failed: {e} — re-arming for next tick");
                            self.software_stop_price = level; // re-arm; retry next tick
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                error!("Software stop: could not read position: {e} — re-arming");
                self.software_stop_price = level;
            }
        }
    }

    /// Open a position and place its TP + SL.
    ///
    /// `fast` = true is the FLIP path: the caller has just confirmed (via a
    /// reduce-only full-size stop fill) that we are flat and has already
    /// cancelled the leftover orders, so we SKIP the extra position-check and
    /// cancel round-trips and go straight to the market order. This is what
    /// makes the flip sub-second on a colocated server.
    async fn open_position(&mut self, direction: &str, price: f64, fast: bool) {
        let t0 = Instant::now();

        if !fast {
            // Entry path (not latency-critical): never open on top of a position.
            match self.rest.position_information(&self.config.symbol).await {
                Ok(positions) => {
                    for p in &positions {
                        let amt = get_f64(p, "positionAmt");
                        if amt != 0.0 {
                            self.current_position =
                                Some(if amt > 0.0 { "long" } else { "short" }.into());
                            self.position_entry_price = Some(get_f64(p, "entryPrice"));
                            warn!("Cannot open {direction} — existing position amount {amt}");
                            return;
                        }
                    }
                }
                Err(e) => warn!("Could not read positions before opening: {e}"),
            }
            // Clear any stale orders first (regular + any leftover algo stops).
            if let Err(e) = self.rest.cancel_all_open_orders(&self.config.symbol).await {
                warn!("Cancel before opening {direction} failed: {e}");
            }
            self.cancel_all_algo_orders("before opening").await;
        }

        let quantity = match self.calculate_quantity(price) {
            Ok(q) => q,
            Err(e) => {
                error!("Quantity calc failed: {e}");
                return;
            }
        };

        let side = if direction == "long" { "BUY" } else { "SELL" };
        // The market order reply carries the real average fill price. TP and SL
        // are measured from that (not from the signal price), so the distances
        // the client configures are the distances he actually gets. No extra
        // API call — the value is already in the response we have.
        let fill = match self
            .rest
            .create_market_order(&self.config.symbol, side, &quantity, false)
            .await
        {
            Ok(v) => {
                // avgPrice when the venue sends it; otherwise derive it from
                // cumQuote/executedQty. Testnet omits avgPrice, live sends it.
                // Falls back to the signal price if neither is present.
                let avg = get_f64(&v, "avgPrice");
                if avg > 0.0 {
                    avg
                } else {
                    let quote = get_f64(&v, "cumQuote");
                    let qty = get_f64(&v, "executedQty");
                    if quote > 0.0 && qty > 0.0 { quote / qty } else { 0.0 }
                }
            }
            Err(e) => {
                // -4164: the order value is under this symbol's minimum. Say
                // what to change rather than repeating Binance's wording —
                // every symbol has its own minimum (BTC is 50 USDT, most
                // small-cap coins are 5).
                if let Some(api) = e.downcast_ref::<ApiError>() {
                    if api.code == -4164 {
                        error!(
                            "{} needs a bigger order than POSITION_SIZE={} at {}x leverage. \
                             Raise POSITION_SIZE in .env (or the leverage) and restart. ({e})",
                            self.config.symbol, self.config.position_size, self.config.leverage
                        );
                        return;
                    }
                }
                error!("Error opening {direction} position: {e}");
                return;
            }
        };

        // Binance does not return avgPrice/cumQuote on this endpoint, so read
        // the position's entryPrice — the exact figure the exchange (and the
        // client) measures the stop distance against. One extra call, and it is
        // what makes the logged percentage match Binance's own numbers.
        let fill = if fill > 0.0 {
            fill
        } else {
            self.read_entry_price(direction, price).await
        };

        self.current_position = Some(direction.into());
        self.position_entry_price = Some(fill);
        info!("Opened {direction} position at {fill} (signal {price}, qty {quantity})");

        if let Err(e) = self.place_take_profit(fill).await {
            error!("Error placing take profit: {e}");
        }
        if let Err(e) = self.place_stop_loss(fill).await {
            error!("Error placing stop loss: {e}");
        }

        // ⏱ How long it took to open + fully protect the position.
        info!(
            "⏱ Position opened + protected in {:.3}s",
            t0.elapsed().as_secs_f64()
        );

        // Stats update is off the timed critical path (position is already safe).
        self.update_balance_and_stats().await;
    }

    /// Read the entry price of the position we just opened, straight from the
    /// exchange — this is the figure Binance shows the user, and the one the
    /// stop distance must be measured from.
    ///
    /// The position must match the side we just opened. Right after a flip the
    /// exchange can still be reporting the OLD position for a few milliseconds;
    /// accepting that would anchor the stop to the previous trade's entry and
    /// put it hundreds of ticks away. Poll briefly for the correct side, and
    /// only fall back to the signal price if it never appears.
    async fn read_entry_price(&self, direction: &str, signal_price: f64) -> f64 {
        let want_long = direction == "long";
        for attempt in 1..=10 {
            match self.rest.position_information(&self.config.symbol).await {
                Ok(positions) => {
                    for p in &positions {
                        let amt = get_f64(p, "positionAmt");
                        let entry = get_f64(p, "entryPrice");
                        if amt != 0.0 && entry > 0.0 && (amt > 0.0) == want_long {
                            return entry;
                        }
                    }
                }
                Err(e) => warn!("Could not read entry price ({e}), attempt {attempt}/10"),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Still nothing. Fall back to the LAST TRADED price, never to the
        // signal price: the signal comes from the mark-price stream, and on
        // thin symbols mark sits ~0.5-0.8% away from last (BANK, AKE), which
        // would put the stop that far out. The order filled at last price, so
        // last price is the closest honest estimate of the entry.
        let last = self
            .rest
            .symbol_price(&self.config.symbol)
            .await
            .unwrap_or(signal_price);
        warn!(
            "Exchange did not report the new {direction} position in time — \
             measuring TP/SL from the last traded price {last} (signal was {signal_price})."
        );
        last
    }

    async fn place_take_profit(&self, entry_price: f64) -> Result<()> {
        let quantity = self.calculate_quantity(entry_price)?;
        let (side, raw_tp) = match self.current_position.as_deref() {
            Some("long") => (
                "SELL",
                entry_price * (1.0 + self.config.take_profit_percentage / 100.0),
            ),
            Some("short") => (
                "BUY",
                entry_price * (1.0 - self.config.take_profit_percentage / 100.0),
            ),
            _ => return Ok(()),
        };
        let tp_price = self.format_price(self.round_to_tick(raw_tp));
        // Straight after a flip the new position can take a moment to register,
        // and a reduce-only order against a position Binance cannot see yet is
        // rejected with -2022. One short retry, otherwise the position runs with
        // no take profit at all.
        let mut placed = self
            .rest
            .create_limit_order(&self.config.symbol, side, &tp_price, &quantity, true)
            .await;
        for backoff_ms in [200_u64, 400, 800] {
            match &placed {
                Err(e) if matches!(e.downcast_ref::<ApiError>(), Some(a) if a.code == -2022) => {
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    placed = self
                        .rest
                        .create_limit_order(&self.config.symbol, side, &tp_price, &quantity, true)
                        .await;
                }
                _ => break,
            }
        }
        placed?;
        info!(
            "Take profit ({:?}) placed at {tp_price} (entry {entry_price})",
            self.current_position
        );
        Ok(())
    }

    /// Place the stop loss as a real exchange order via the CONDITIONAL Algo
    /// endpoint (required since Binance's 2025-12-09 migration — the old
    /// endpoint returns -4120 for stop orders). The trigger is always measured
    /// from the position's real entry fill, so the loss is exactly
    /// STOP_LOSS_PERCENTAGE; a -2021 ("would immediately trigger") retry may
    /// shift it by a tick or two but never by another whole percentage, and
    /// never past 2x the configured distance.
    async fn place_stop_loss(&mut self, entry_price: f64) -> Result<()> {
        let quantity = self.calculate_quantity(entry_price)?;
        let max_retries = 5;
        // Start clean.
        self.software_stop_price = None;
        self.stop_algo_id = None;

        let pct = self.config.stop_loss_percentage;
        let is_long = match self.current_position.as_deref() {
            Some("long") => true,
            Some("short") => false,
            _ => return Ok(()),
        };
        let side = if is_long { "SELL" } else { "BUY" };

        // The stop is ALWAYS measured from the position's entry price, so the
        // risk is exactly STOP_LOSS_PERCENTAGE regardless of how many retries
        // happen. A retry may nudge it by a tick or two, never by another whole
        // stop-loss distance.
        let target = if is_long {
            entry_price * (1.0 - pct / 100.0)
        } else {
            entry_price * (1.0 + pct / 100.0)
        };
        // Absolute ceiling on the stop distance: the configured distance, plus
        // two ticks of tolerance for rounding. The stop is NEVER placed wider
        // than STOP_LOSS_PERCENTAGE. If price has already run past that level
        // the loss has already been taken, and a wider stop would only enlarge
        // it — so the bot closes at market instead (see the -2021 arm below).
        let max_dist = (entry_price * pct / 100.0) + 2.0 * self.filters.tick_size;

        let mut nudge_ticks = 2.0_f64;
        let mut sl_price = self.round_to_tick(target);
        // A stop closer than one tick would round back onto the entry price and
        // be rejected. Push it to the nearest legal tick and say so, because it
        // means the configured percentage is finer than this symbol can express.
        if (sl_price - entry_price).abs() < self.filters.tick_size {
            sl_price = self.round_to_tick(if is_long {
                entry_price - self.filters.tick_size
            } else {
                entry_price + self.filters.tick_size
            });
            warn!(
                "STOP_LOSS_PERCENTAGE={pct}% is smaller than one tick ({}) on {} — \
                 using the closest possible stop {} ({:.3}%).",
                self.filters.tick_size,
                self.config.symbol,
                self.format_price(sl_price),
                (sl_price - entry_price).abs() / entry_price * 100.0
            );
        }
        for attempt in 1..=max_retries {
            let trigger = self.format_price(sl_price);
            match self
                .rest
                .create_algo_stop_market(&self.config.symbol, side, &trigger, &quantity, true)
                .await
            {
                Ok(algo_id) => {
                    self.stop_algo_id = Some(algo_id);
                    info!(
                        "Placed stop loss at {trigger} = {:.3}% from entry {entry_price} \
                         — real exchange order (algoId {algo_id}, attempt {attempt})",
                        (sl_price - entry_price).abs() / entry_price * 100.0
                    );
                    return Ok(());
                }
                Err(e) => {
                    if let Some(api) = e.downcast_ref::<ApiError>() {
                        // -2021 = Binance says this trigger would fire at
                        // once. That has two very different causes, and they
                        // need opposite responses.
                        if api.code == -2021 {
                            let cur = self
                                .rest
                                .symbol_price(&self.config.symbol)
                                .await
                                .unwrap_or(entry_price);

                            // (a) Price really has run through the stop level —
                            // the loss is already taken. Widening would only
                            // enlarge it, so close now.
                            let already_past =
                                if is_long { cur <= target } else { cur >= target };
                            if already_past {
                                error!(
                                    "Price ({cur}) has already run past the stop-loss level {trigger} \
                                     ({pct}% from entry {entry_price}) — closing at market now. The \
                                     stop is never widened beyond STOP_LOSS_PERCENTAGE."
                                );
                                self.emergency_close().await;
                                return Ok(());
                            }

                            // (b) The level is still on the correct side of the
                            // last price, but Binance refuses it anyway — it
                            // validates the trigger against the MARK price, and
                            // on thin symbols mark sits far from last (BANK/AKE
                            // ~0.5-0.8%). Nudge outward, doubling each attempt,
                            // but NEVER inside the configured distance: pulling
                            // the stop closer would stop the user out early on a
                            // distance they did not ask for.
                            let nudge = nudge_ticks * self.filters.tick_size;
                            nudge_ticks *= 2.0;
                            let candidate = self.round_to_tick(if is_long {
                                (cur - nudge).min(target)
                            } else {
                                (cur + nudge).max(target)
                            });
                            if (candidate - entry_price).abs() > max_dist {
                                // Cannot satisfy the exchange without exceeding
                                // the risk the user configured. Keep the exact
                                // configured level and enforce it from the bot.
                                self.software_stop_price = Some(self.round_to_tick(target));
                                warn!(
                                    "Binance will not accept a stop at {pct}% from entry {entry_price} \
                                     (it validates against mark price, which is far from last on this \
                                     symbol). Holding the exact {pct}% level {} as a bot-side stop \
                                     instead of widening it.",
                                    self.format_price(target)
                                );
                                return Ok(());
                            }
                            sl_price = candidate;
                            warn!(
                                "Stop loss at {trigger} rejected by Binance (last price {cur}). \
                                 Re-placing at {} ({attempt}/{max_retries}).",
                                self.format_price(sl_price)
                            );
                            continue;
                        }
                    }
                    // Exchange truly refused the stop → last-resort software stop.
                    self.software_stop_price = Some(sl_price);
                    warn!(
                        "Could not place exchange stop ({e}). Using bot-side software \
                         stop-loss at {trigger} as a fallback."
                    );
                    return Ok(());
                }
            }
        }

        error!("Could not place a stop loss after {max_retries} attempts — emergency closing.");
        self.emergency_close().await;
        Ok(())
    }

    /// Cancel the exchange-side algo stop-loss (if any) and clear its id.
    /// Fast (one call) — used on the latency-sensitive flip path.
    async fn cancel_stop_algo(&mut self) {
        if let Some(id) = self.stop_algo_id.take() {
            if let Err(e) = self.rest.cancel_algo_order(id).await {
                // -2011 "unknown order" is EXPECTED when the stop just fired
                // (it's already gone) — not an error, so don't alarm with a warn.
                if e.downcast_ref::<ApiError>().map(|a| a.code) != Some(-2011) {
                    warn!("Cancel algo stop {id}: {e}");
                }
            }
        }
    }

    /// Cancel EVERY resting algo (conditional) stop order for this symbol —
    /// robust cleanup that also clears leftovers from manual closes or past
    /// orphans. Used on non-latency-critical paths (TP, shutdown, consistency).
    async fn cancel_all_algo_orders(&mut self, context: &str) {
        match self.rest.open_algo_orders(&self.config.symbol).await {
            Ok(orders) => {
                if !orders.is_empty() {
                    info!("Cancelling {} leftover algo stop order(s) ({context})", orders.len());
                }
                for o in &orders {
                    if let Some(id) = get_i64(o, "algoId") {
                        if let Err(e) = self.rest.cancel_algo_order(id).await {
                            if e.downcast_ref::<ApiError>().map(|a| a.code) != Some(-2011) {
                                warn!("Cancel algo {id} ({context}): {e}");
                            }
                        }
                    }
                }
            }
            Err(e) => warn!("List algo orders failed ({context}): {e}"),
        }
        self.stop_algo_id = None;
    }

    /// Cancel every open order and VERIFY the book is empty, retrying a few
    /// times. Mirrors the Python `_ensure_all_orders_cancelled`. The happy path
    /// is one cancel + one check (fast); only a non-empty book triggers a short
    /// 150 ms retry, so the flip stays fast under normal conditions but never
    /// silently leaves a stray order behind on live money.
    async fn ensure_orders_cancelled(&self, context: &str, max_passes: u32) {
        for pass in 1..=max_passes {
            if let Err(e) = self.rest.cancel_all_open_orders(&self.config.symbol).await {
                warn!("Cancel-all failed ({context}, pass {pass}): {e}");
            }
            match self.rest.open_orders(&self.config.symbol).await {
                Ok(orders) if orders.is_empty() => {
                    if pass > 1 {
                        info!("All open orders cancelled ({context})");
                    }
                    return;
                }
                Ok(orders) => warn!(
                    "{} order(s) still open after cancel ({context}) — retry {pass}/{max_passes}",
                    orders.len()
                ),
                Err(e) => warn!("Could not verify open orders ({context}): {e}"),
            }
            if pass < max_passes {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
    }

    async fn emergency_close(&mut self) {
        if self.current_position.is_none() {
            return;
        }
        let _ = self.rest.cancel_all_open_orders(&self.config.symbol).await;
        self.cancel_all_algo_orders("emergency close").await;
        if let Ok(positions) = self.rest.position_information(&self.config.symbol).await {
            for p in &positions {
                let amt = get_f64(p, "positionAmt");
                if amt != 0.0 {
                    let side = if amt > 0.0 { "SELL" } else { "BUY" };
                    let qty = self.format_quantity(amt.abs());
                    if let Err(e) = self
                        .rest
                        .create_market_order(&self.config.symbol, side, &qty, true)
                        .await
                    {
                        error!("Emergency close failed: {e}");
                    } else {
                        warn!("Emergency-closed {:?} position ({} units)", self.current_position, qty);
                    }
                }
            }
        }
        self.current_position = None;
        self.position_entry_price = None;
    }

    // -----------------------------------------------------------------
    // Order-fill handling: stop-loss flip, take-profit stop
    // -----------------------------------------------------------------
    async fn on_order_fill(&mut self, o: OrderUpdate) {
        if o.symbol != self.config.symbol || o.status != "FILLED" {
            return;
        }
        info!("Order executed: {} {} for {}", o.order_type, o.side, o.symbol);

        let is_closing_side = matches!(
            (self.current_position.as_deref(), o.side.as_str()),
            (Some("long"), "SELL") | (Some("short"), "BUY")
        );
        let stop_loss_triggered = o.reduce_only
            && is_closing_side
            && (o.order_type == "MARKET" || o.order_type == "STOP_MARKET");

        if stop_loss_triggered {
            // ⏱ Start the flip timer the instant we detect the stop fill.
            let t_flip = Instant::now();
            info!("Stop loss execution detected");
            let previous = self.current_position.clone();
            self.current_position = None;
            self.position_entry_price = None;
            self.software_stop_price = None;
            // Cancel the tracked algo stop: harmless (-2011) if it just fired,
            // but removes it if this close came from elsewhere (e.g. manual).
            self.cancel_stop_algo().await;

            // Use the latest WebSocket price instead of a REST round-trip.
            let current_price = match self.last_price {
                Some(p) if p > 0.0 => p,
                _ => self
                    .rest
                    .symbol_price(&self.config.symbol)
                    .await
                    .unwrap_or(0.0),
            };
            self.entry_price = Some(current_price);

            // The stop was a reduce-only, full-size close, so the position is
            // already flat — no position re-check needed. Cancel the leftover TP
            // (one call) and flip straight into the opposite side (fast path).
            let _ = self.rest.cancel_all_open_orders(&self.config.symbol).await;

            match previous.as_deref() {
                Some("long") => {
                    info!("Flipping from long to short at {current_price}");
                    self.open_position("short", current_price, true).await;
                }
                Some("short") => {
                    info!("Flipping from short to long at {current_price}");
                    self.open_position("long", current_price, true).await;
                }
                _ => {}
            }

            // ⏱ Total time from detecting the stop to the new position being open + protected.
            info!(
                "⏱ FLIP completed in {:.3}s (detect → opposite position open + protected)",
                t_flip.elapsed().as_secs_f64()
            );
            // Stats update off the critical path.
            self.update_balance_and_stats().await;
        } else if o.order_type == "LIMIT" && o.reduce_only {
            // Take profit hit -> stop the bot (client requirement #6).
            info!("Take profit executed: {} for {}", o.side, o.symbol);
            self.current_position = None;
            self.position_entry_price = None;
            self.software_stop_price = None;
            self.update_balance_and_stats().await;
            // Cancel ALL resting orders (regular + every algo stop) before stopping.
            self.cancel_all_algo_orders("after take profit").await;
            self.ensure_orders_cancelled("after take profit", 10).await;

            let total = self.current_balance - self.initial_balance;
            let pct = if self.initial_balance > 0.0 {
                (self.current_balance / self.initial_balance - 1.0) * 100.0
            } else {
                0.0
            };
            info!(
                "\n=======================================\nTAKE PROFIT HIT - STOPPING BOT\n\
                 =======================================\nFinal Balance: {:.4} USDT\n\
                 Total Profit: {total:+.4} USDT ({pct:+.2}%)\nProfitable Trades: {}/{}\n\
                 =======================================",
                self.current_balance, self.profitable_trades, self.trades_count
            );
            self.stop_flag = true;
        }
    }

    // -----------------------------------------------------------------
    // Consistency check + heartbeat (were background threads in Python)
    // -----------------------------------------------------------------
    async fn consistency_check(&mut self) {
        // Reconcile tracked vs. actual position.
        let positions = match self.rest.position_information(&self.config.symbol).await {
            Ok(p) => p,
            Err(e) => {
                error!("Consistency check position read failed: {e}");
                return;
            }
        };
        let mut actual: Option<String> = None;
        let mut actual_entry: Option<f64> = None;
        for p in &positions {
            let amt = get_f64(p, "positionAmt");
            if amt != 0.0 {
                actual = Some(if amt > 0.0 { "long" } else { "short" }.into());
                actual_entry = Some(get_f64(p, "entryPrice"));
                break;
            }
        }
        if actual != self.current_position {
            warn!(
                "Position mismatch: tracked={:?}, actual={:?} — syncing",
                self.current_position, actual
            );
            self.current_position = actual.clone();
            self.position_entry_price = actual_entry;
        }

        // Orphan-order cleanup. With a position, there should be just the TP
        // limit here (the SL is a separate algo order). A single edited TP is
        // left alone (client req #8); extra piled-up orders get cleaned. With no
        // position, everything is cancelled (handles manual closes / orphans).
        let open_orders = match self.rest.open_orders(&self.config.symbol).await {
            Ok(o) => o,
            Err(e) => {
                error!("Consistency check open-orders read failed: {e}");
                return;
            }
        };
        if self.current_position.is_some() {
            // With a position there should be just the TP limit (the SL is a
            // separate algo order). Only clean up if extra REGULAR orders pile up.
            if open_orders.len() > 1 {
                warn!("{} regular orders with a position — cleaning up and re-placing TP/SL", open_orders.len());
                let _ = self.rest.cancel_all_open_orders(&self.config.symbol).await;
                self.cancel_all_algo_orders("re-arm with position").await;
                self.ensure_position_orders().await;
            }
        } else {
            // No position → cancel ALL leftover orders: regular (TP) AND algo
            // (SL). This is what cleans up after a MANUAL close (client's
            // request) or any orphaned stop orders that piled up.
            if !open_orders.is_empty() {
                warn!("{} orphan regular order(s) with no position — cancelling", open_orders.len());
                let _ = self.rest.cancel_all_open_orders(&self.config.symbol).await;
            }
            self.cancel_all_algo_orders("no position").await;
        }
    }

    async fn ensure_position_orders(&mut self) {
        let entry = match self.position_entry_price {
            Some(e) => e,
            None => return,
        };
        if let Err(e) = self.place_take_profit(entry).await {
            error!("Re-place TP failed: {e}");
        }
        if let Err(e) = self.place_stop_loss(entry).await {
            error!("Re-place SL failed: {e}");
        }
    }

    fn heartbeat(&self) {
        let mut line = format!(
            "Heartbeat - Position: {:?} - Balance: {:.4}",
            self.current_position, self.current_balance
        );
        if let (Some(price), Some(_)) = (self.last_price, self.entry_price) {
            let change = self.price_change_pct(price);
            line.push_str(&format!(
                " - Price: {price} ({change:+.2}% from ref {:?})",
                self.entry_price
            ));
            if self.use_custom_entry && self.current_position.is_none() {
                line.push_str(&format!(
                    " - Waiting for range LOW={:?}/HIGH={:?}",
                    self.config.custom_entry_price_low, self.config.custom_entry_price_high
                ));
            } else if let Some(dir) = &self.watch_direction {
                line.push_str(&format!(
                    " - Watching {} (need {}% to confirm)",
                    dir.to_uppercase(),
                    self.config.confirm_percentage
                ));
            }
        }
        info!("{line}");
    }

    // -----------------------------------------------------------------
    // Balance / stats
    // -----------------------------------------------------------------
    async fn update_balance_and_stats(&mut self) {
        let new_balance = match self.rest.balance_usdt().await {
            Ok(b) => b,
            Err(e) => {
                error!("Balance read failed: {e}");
                return;
            }
        };
        if (new_balance - self.current_balance).abs() > f64::EPSILON {
            let profit = new_balance - self.current_balance;
            let total = new_balance - self.initial_balance;
            if profit > 0.0 {
                self.profitable_trades += 1;
            }
            self.trades_count += 1;
            let pct = if self.initial_balance > 0.0 {
                (new_balance / self.initial_balance - 1.0) * 100.0
            } else {
                0.0
            };
            info!(
                "\n=== Trade Stats ===\nLast trade P/L: {profit:+.4} USDT\nCurrent Balance: {new_balance:.4} USDT\n\
                 Total P/L: {total:+.4} USDT ({pct:+.2}%)\nProfitable Trades: {}/{}\n=================",
                self.profitable_trades, self.trades_count
            );
            self.current_balance = new_balance;
        }
    }

    // -----------------------------------------------------------------
    // Quantity / price precision helpers (ports the Python size handling
    // that a previous dev fixed — including the min-notional bump)
    // -----------------------------------------------------------------
    fn calculate_quantity(&self, price: f64) -> Result<String> {
        // Per-symbol minimum, read from the exchange at start-up. BTC is 50
        // USDT, ETH 20, most alt-coins 5 — a hardcoded value silently breaks
        // whenever the user switches to a bigger-minimum symbol.
        let min_notional = self.filters.min_notional * 1.02; // small buffer
        let raw_quantity = self.config.position_size / price;
        let min_quantity = min_notional / price;
        let mut quantity = raw_quantity.max(min_quantity);

        let step = self.filters.step_size;
        // Round DOWN to a valid step first.
        quantity = (quantity / step).floor() * step;
        if quantity <= 0.0 {
            quantity = step;
        }

        // Bump up if flooring dropped us below the exchange minimum.
        if quantity * price < min_notional {
            quantity = (min_notional / price / step).ceil() * step;
        }

        let formatted = self.format_quantity(quantity);
        let final_notional = formatted.parse::<f64>().unwrap_or(0.0) * price;
        // The exchange minimum can force a bigger trade than POSITION_SIZE asks
        // for. Never let that happen silently — it is the user's money.
        if final_notional > self.config.position_size * 1.05 {
            warn!(
                "{} will trade {:.2} USDT, not POSITION_SIZE={} — this symbol's minimum order is {:.2} USDT.",
                self.config.symbol, final_notional, self.config.position_size, self.filters.min_notional
            );
        }
        if final_notional < min_notional {
            return Err(anyhow::anyhow!(
                "{} requires orders of at least {:.2} USDT; POSITION_SIZE={} gives only {final_notional:.2} USDT. \
                 Raise POSITION_SIZE in .env.",
                self.config.symbol, self.filters.min_notional, self.config.position_size
            ));
        }
        Ok(formatted)
    }

    fn round_to_tick(&self, price: f64) -> f64 {
        let t = self.filters.tick_size;
        if t <= 0.0 {
            price
        } else {
            (price / t).round() * t
        }
    }

    fn format_price(&self, price: f64) -> String {
        format!("{:.*}", self.filters.price_precision, price)
    }

    fn format_quantity(&self, qty: f64) -> String {
        format!("{:.*}", self.filters.quantity_precision, qty)
    }

    // -----------------------------------------------------------------
    // Graceful shutdown (Ctrl+C): cancel orders, close open position.
    // -----------------------------------------------------------------
    pub async fn shutdown(&mut self) {
        info!("Stopping bot...");
        self.cancel_all_algo_orders("bot shutdown").await;
        self.ensure_orders_cancelled("bot shutdown", 5).await;
        if self.current_position.is_some() {
            // Close whatever is actually open at market.
            if let Ok(positions) = self.rest.position_information(&self.config.symbol).await {
                for p in &positions {
                    let amt = get_f64(p, "positionAmt");
                    if amt != 0.0 {
                        let side = if amt > 0.0 { "SELL" } else { "BUY" };
                        let qty = self.format_quantity(amt.abs());
                        if let Err(e) = self
                            .rest
                            .create_market_order(&self.config.symbol, side, &qty, true)
                            .await
                        {
                            error!("Close on shutdown failed: {e}");
                        } else {
                            info!("Closed {qty} units at market on shutdown");
                        }
                    }
                }
            }
            self.current_position = None;
            self.position_entry_price = None;
        }
        let _ = self.rest.close_listen_key().await;
        info!("Bot stopped");
    }
}

/// Free helper so `main` can log a position snapshot if needed later.
#[allow(dead_code)]
pub fn position_amt(p: &Value) -> f64 {
    get_f64(p, "positionAmt")
}
