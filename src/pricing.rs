//! Pure price math for stop-loss, take-profit and tick rounding.
//!
//! Everything here is I/O-free on purpose: this is the code the client's money
//! depends on, so it is unit-tested against the exact trades he reported as
//! wrong (see the tests at the bottom of this file).
//!
//! The rules, in one place, for every symbol and both directions:
//!
//!   LONG   stop loss   = entry * (1 - sl% / 100)      (below entry)
//!   SHORT  stop loss   = entry * (1 + sl% / 100)      (above entry)
//!   LONG   take profit = entry * (1 + tp% / 100)      (above entry)
//!   SHORT  take profit = entry * (1 - tp% / 100)      (below entry)
//!
//! `entry` is ALWAYS the average price this position's own market order
//! actually filled at. It is never the mark price, never the signal price and
//! never a blended position average — those are what made the stop land at
//! 0.62% when 0.10% was configured.
//!
//! Leverage does NOT appear in any of these formulas, and must not: leverage
//! changes the ROI of a move, not the price of it. A 1% stop is 1% of price at
//! 1x and at 20x.

/// Decimal places implied by a Binance size string, e.g. `"0.0000100"` -> 5,
/// `"0.10"` -> 1, `"1"` -> 0, `"2.5"` -> 1.
///
/// Read from the STRING, not from `-log10(value)`. `log10` silently gives the
/// wrong answer for any tick that is not a clean power of ten (`0.005` -> 2
/// instead of 3), and a price formatted with too few decimals is rounded onto
/// a coarser grid than the symbol actually uses — which moves the stop.
pub fn decimals_from_size_str(s: &str) -> Option<usize> {
    let s = s.trim();
    // Scientific notation would need different handling; Binance does not use
    // it in exchangeInfo, so bail out and let the caller fall back.
    if s.contains(['e', 'E']) {
        return None;
    }
    match s.split_once('.') {
        None => Some(0),
        Some((_, frac)) => Some(frac.trim_end_matches('0').len()),
    }
}

/// Fallback for when the raw string is unavailable: decimals implied by the
/// numeric size. Only exact for powers of ten, hence the string version above.
pub fn decimals_from_size(size: f64) -> usize {
    if size <= 0.0 {
        return 0;
    }
    let p = (-size.log10()).round();
    if p < 0.0 {
        0
    } else {
        p as usize
    }
}

/// Snap a price onto the symbol's tick grid, rounding to the NEAREST tick
/// (never flooring, never a fixed offset).
pub fn round_to_tick(price: f64, tick: f64) -> f64 {
    if tick <= 0.0 || !tick.is_finite() {
        return price;
    }
    // Nudge by a relative epsilon first: a value that is mathematically exactly
    // half a tick can land on ...49999999 in binary floating point and round
    // the wrong way. The nudge is ~1e-9 of the ratio — far too small to move a
    // price that is not already sitting on the boundary.
    let ratio = price / tick;
    let ratio = ratio + ratio.abs() * 1e-9;
    ratio.round() * tick
}

/// The stop-loss price for a position, before tick rounding.
pub fn stop_loss_target(entry: f64, sl_pct: f64, is_long: bool) -> f64 {
    if is_long {
        entry * (1.0 - sl_pct / 100.0)
    } else {
        entry * (1.0 + sl_pct / 100.0)
    }
}

/// The take-profit price for a position, before tick rounding.
pub fn take_profit_target(entry: f64, tp_pct: f64, is_long: bool) -> f64 {
    if is_long {
        entry * (1.0 + tp_pct / 100.0)
    } else {
        entry * (1.0 - tp_pct / 100.0)
    }
}

/// Distance of `price` from `entry`, as a positive percentage of `entry`.
/// This is the number the client reads off Binance, so it is what the bot
/// logs and what the tests assert on.
pub fn distance_pct(entry: f64, price: f64) -> f64 {
    if entry <= 0.0 {
        0.0
    } else {
        (price - entry).abs() / entry * 100.0
    }
}

/// Format a price with the symbol's own number of decimals.
pub fn format_price(price: f64, decimals: usize) -> String {
    format!("{:.*}", decimals, price)
}

/// The complete stop-loss decision for one position: snap the target onto the
/// tick grid, and if the configured percentage is finer than a single tick,
/// use the closest tick that is still a real stop instead of silently
/// collapsing onto the entry price.
///
/// Returns `(sl_price, hit_tick_floor)`.
pub fn stop_loss_price(entry: f64, sl_pct: f64, is_long: bool, tick: f64) -> (f64, bool) {
    let target = stop_loss_target(entry, sl_pct, is_long);
    let mut sl = round_to_tick(target, tick);
    let mut hit_tick_floor = false;
    if tick > 0.0 && (sl - entry).abs() < tick {
        sl = round_to_tick(
            if is_long { entry - tick } else { entry + tick },
            tick,
        );
        hit_tick_floor = true;
    }
    (sl, hit_tick_floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real Binance filters, read from /fapi/v1/exchangeInfo.
    const BANK_TICK: f64 = 0.0000100; // BANKUSDT — the symbol that was wrong
    const SOL_TICK: f64 = 0.0100; // SOLUSDT  — the symbol that was right
    const BTC_TICK: f64 = 0.10;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    // -- decimals ----------------------------------------------------------

    #[test]
    fn decimals_come_from_the_tick_string_exactly() {
        assert_eq!(decimals_from_size_str("0.0000100"), Some(5)); // BANKUSDT
        assert_eq!(decimals_from_size_str("0.0100"), Some(2)); // SOLUSDT
        assert_eq!(decimals_from_size_str("0.10"), Some(1)); // BTCUSDT
        assert_eq!(decimals_from_size_str("0.0000001"), Some(7)); // AKEUSDT
        assert_eq!(decimals_from_size_str("1"), Some(0)); // step size
        assert_eq!(decimals_from_size_str("0.001"), Some(3));
    }

    #[test]
    fn log10_decimals_are_wrong_for_non_power_of_ten_ticks() {
        // The old bot derived decimals with -log10(tick). For a tick like
        // 0.005 that gives 2 decimals, so 0.15973 would be sent as "0.16" —
        // a 0.17% jump on a 0.16 price. The string version gets it right.
        assert_eq!(decimals_from_size(0.005), 2); // wrong, by design of log10
        assert_eq!(decimals_from_size_str("0.005"), Some(3)); // right
        assert_eq!(decimals_from_size(2.5), 0);
        assert_eq!(decimals_from_size_str("2.5"), Some(1));
    }

    // -- tick rounding -----------------------------------------------------

    #[test]
    fn rounds_to_nearest_tick_not_floor() {
        // 0.159739 is closer to 0.15974 than to 0.15973.
        assert!(approx(round_to_tick(0.159739, BANK_TICK), 0.15974, 1e-9));
        assert!(approx(round_to_tick(0.159731, BANK_TICK), 0.15973, 1e-9));
        assert!(approx(round_to_tick(78.2649, SOL_TICK), 78.26, 1e-9));
        assert!(approx(round_to_tick(78.2651, SOL_TICK), 78.27, 1e-9));
    }

    #[test]
    fn rounded_price_is_always_a_whole_number_of_ticks() {
        for entry in [0.1598900_f64, 0.1478997, 0.1669348, 0.2334600] {
            for pct in [0.06_f64, 0.1, 0.15, 0.25, 0.3, 1.0, 1.5, 2.0] {
                for is_long in [true, false] {
                    let (sl, _) = stop_loss_price(entry, pct, is_long, BANK_TICK);
                    let ticks = sl / BANK_TICK;
                    assert!(
                        (ticks - ticks.round()).abs() < 1e-6,
                        "entry {entry} pct {pct} long {is_long} -> {sl} is not on the tick grid"
                    );
                }
            }
        }
    }

    // -- direction ---------------------------------------------------------

    #[test]
    fn long_stop_is_below_entry_and_short_stop_is_above() {
        let entry = 0.1598900;
        let (long_sl, _) = stop_loss_price(entry, 0.1, true, BANK_TICK);
        let (short_sl, _) = stop_loss_price(entry, 0.1, false, BANK_TICK);
        assert!(long_sl < entry, "long stop {long_sl} must sit below {entry}");
        assert!(short_sl > entry, "short stop {short_sl} must sit above {entry}");
    }

    #[test]
    fn long_take_profit_is_above_entry_and_short_below() {
        let entry = 78.3400;
        assert!(take_profit_target(entry, 2.0, true) > entry);
        assert!(take_profit_target(entry, 2.0, false) < entry);
    }

    // -- the client's actual trades ---------------------------------------
    //
    // These are the real entries the client reported, with the stop distance
    // the bot actually produced. Each case asserts the placed stop is within
    // ONE TICK of the configured percentage — the guarantee that was broken.

    fn assert_within_one_tick(entry: f64, pct: f64, is_long: bool, tick: f64) {
        let (sl, floored) = stop_loss_price(entry, pct, is_long, tick);
        assert!(!floored, "entry {entry} pct {pct}: should not need the tick floor");
        let actual = distance_pct(entry, sl);
        let tolerance = tick / entry * 100.0; // one tick, expressed as a percent
        assert!(
            (actual - pct).abs() <= tolerance,
            "entry {entry}, configured {pct}%, placed {sl} = {actual:.4}% \
             (allowed +/- {tolerance:.4}%)"
        );
    }

    #[test]
    fn bank_trade_1_configured_0_1_percent() {
        // Client saw 0.6192% (stop at 0.15890). Correct answer is 0.15973.
        let (sl, _) = stop_loss_price(0.1598900, 0.1, true, BANK_TICK);
        assert!(approx(sl, 0.15973, 1e-9), "got {sl}");
        assert!(distance_pct(0.1598900, sl) < 0.11);
        assert_within_one_tick(0.1598900, 0.1, true, BANK_TICK);
    }

    #[test]
    fn bank_trade_2_configured_1_percent() {
        // Client saw 2.5623% (stop at 0.144110). Correct answer is 0.14642.
        let (sl, _) = stop_loss_price(0.1478997, 1.0, true, BANK_TICK);
        assert!(approx(sl, 0.14642, 1e-9), "got {sl}");
        assert_within_one_tick(0.1478997, 1.0, true, BANK_TICK);
    }

    #[test]
    fn bank_trade_3_configured_1_5_percent() {
        // Client saw 1.7700% (stop at 0.163980). Correct answer is 0.16443.
        let (sl, _) = stop_loss_price(0.1669348, 1.5, true, BANK_TICK);
        assert!(approx(sl, 0.16443, 1e-9), "got {sl}");
        assert_within_one_tick(0.1669348, 1.5, true, BANK_TICK);
    }

    #[test]
    fn bank_trade_4_short_configured_0_3_percent() {
        // Client saw 1.3279% (stop at 0.2365600). Correct answer is 0.23416.
        let (sl, _) = stop_loss_price(0.2334600, 0.3, false, BANK_TICK);
        assert!(approx(sl, 0.23416, 1e-9), "got {sl}");
        assert_within_one_tick(0.2334600, 0.3, false, BANK_TICK);
    }

    #[test]
    fn every_configured_percentage_the_client_used_is_honoured() {
        // 0.06 / 0.10 / 0.15 / 0.25 / 1.00 / 1.50 — the settings from the chat.
        for pct in [0.06_f64, 0.10, 0.15, 0.25, 1.00, 1.50] {
            for is_long in [true, false] {
                assert_within_one_tick(0.1598900, pct, is_long, BANK_TICK);
                assert_within_one_tick(78.3400, pct, is_long, SOL_TICK);
            }
        }
    }

    #[test]
    fn small_percentages_are_not_amplified_on_cheap_symbols() {
        // The reported failure mode: the smaller the configured stop, the
        // bigger the relative error (0.06% -> 0.30%, 0.10% -> 0.62%). The
        // error must stay bounded by one tick at every size.
        let entry = 0.1598900;
        for pct in [0.06_f64, 0.08, 0.10, 0.12, 0.15, 0.20, 0.30] {
            let (sl, _) = stop_loss_price(entry, pct, true, BANK_TICK);
            let actual = distance_pct(entry, sl);
            assert!(
                actual < pct * 1.2 + 0.01,
                "configured {pct}% blew up to {actual:.4}%"
            );
        }
    }

    #[test]
    fn sol_and_bank_behave_identically_in_relative_terms() {
        // The client's key observation was that SOLUSDT was right while
        // BANKUSDT was wrong. Price scale must not change the outcome.
        for pct in [0.1_f64, 0.5, 1.0, 1.5] {
            let sol = distance_pct(78.3400, stop_loss_price(78.3400, pct, true, SOL_TICK).0);
            let bank =
                distance_pct(0.1598900, stop_loss_price(0.1598900, pct, true, BANK_TICK).0);
            let btc = distance_pct(95000.0, stop_loss_price(95000.0, pct, true, BTC_TICK).0);
            for (name, got) in [("SOL", sol), ("BANK", bank), ("BTC", btc)] {
                assert!(
                    (got - pct).abs() < 0.02,
                    "{name} at {pct}% produced {got:.4}%"
                );
            }
        }
    }

    // -- leverage independence --------------------------------------------

    #[test]
    fn leverage_never_enters_the_stop_price() {
        // There is no leverage argument to any of these functions, and there
        // must never be one. This test documents the invariant: the stop for
        // a 1% setting is the same price at 1x, 2x and 20x — only the ROI
        // differs. If someone later adds a leverage term, this fails.
        let entry = 100.0;
        let expected = 99.0;
        for _leverage in [1u32, 2, 5, 20, 125] {
            let (sl, _) = stop_loss_price(entry, 1.0, true, 0.01);
            assert!(approx(sl, expected, 1e-9));
        }
    }

    // -- percentage-unit sanity -------------------------------------------

    #[test]
    fn percentages_are_percent_units_not_fractions() {
        // 1.0 means 1%, not 100% and not 0.01%.
        assert!(approx(stop_loss_target(100.0, 1.0, true), 99.0, 1e-9));
        assert!(approx(stop_loss_target(100.0, 0.1, true), 99.9, 1e-9));
        assert!(approx(stop_loss_target(100.0, 0.06, true), 99.94, 1e-9));
        assert!(approx(stop_loss_target(100.0, 1.0, false), 101.0, 1e-9));
    }

    #[test]
    fn no_fixed_absolute_offset_anywhere() {
        // A hardcoded offset (entry - 0.001) would give the same absolute
        // distance on every symbol. Assert the distance scales with price.
        let d_small = 0.16 - stop_loss_target(0.16, 1.0, true);
        let d_large = 100.0 - stop_loss_target(100.0, 1.0, true);
        assert!(
            (d_large / d_small - 100.0 / 0.16).abs() < 1e-6,
            "stop distance is not proportional to price"
        );
    }

    // -- sub-tick guard ----------------------------------------------------

    #[test]
    fn percentage_finer_than_one_tick_is_reported_not_silently_widened() {
        // 0.001% of 0.16 is 0.0000016 — a sixth of a BANK tick. The stop must
        // land exactly one tick away and say so, never zero and never wide.
        let entry = 0.1598900;
        let (sl, floored) = stop_loss_price(entry, 0.001, true, BANK_TICK);
        assert!(floored, "should report that it hit the tick floor");
        assert!(approx(sl, entry - BANK_TICK, 1e-7), "got {sl}");
        assert!(sl < entry);

        let (sl_short, floored_short) = stop_loss_price(entry, 0.001, false, BANK_TICK);
        assert!(floored_short);
        assert!(sl_short > entry);
    }
}
