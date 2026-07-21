//! Entry point. Mirrors `main.py`:
//!   1. ask the user for the entry mode (custom price range or watch/confirm),
//!   2. load config from `.env`,
//!   3. set leverage + margin type, read symbol precision,
//!   4. start the price WS, user-data WS, keepalive, and periodic timers,
//!   5. run the single-owner event loop until take-profit or Ctrl+C.

mod bot;
mod config;
mod events;
mod rest;
mod websocket;

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::bot::TradingBot;
use crate::config::TradingConfig;
use crate::events::Event;
use crate::rest::BinanceRest;

/// Bump this whenever a change ships, so the running binary can be identified
/// from its start-up line — a stale binary is otherwise indistinguishable.
const BUILD_VERSION: &str = "2026-07-21.7";

#[tokio::main]
async fn main() -> Result<()> {
    // INFO logging by default; override with RUST_LOG (e.g. RUST_LOG=debug).
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    // --- 1. decide entry mode: from .env (works for the 24/7 service), or an
    //         interactive prompt when run by hand in a real terminal ---
    // Build stamp. If this line is missing or shows an older version, the
    // binary being run is stale — the source was updated but not rebuilt with
    // `cargo build --release`.
    println!("\nBot version: {} (SL-accuracy build)", BUILD_VERSION);

    let _ = dotenvy::dotenv(); // load .env so ENTRY_MODE etc. are visible here
    let (low, high) = resolve_entry_mode()?;

    // --- 2. config from .env ---
    let cfg = TradingConfig::load(low, high)?;
    println!("\nTrading mode: {}", cfg.env_name());
    if cfg.use_testnet {
        println!("Using Binance Futures TESTNET — free keys (no KYC) from https://testnet.binancefuture.com");
    } else if cfg.use_demo {
        println!("Using Binance demo — keys from https://demo.binance.com");
    } else {
        println!("WARNING: Live trading — real money at risk");
    }

    // --- 3. REST client + account setup ---
    let rest = Arc::new(BinanceRest::new(
        cfg.fapi_base(),
        &cfg.api_key,
        &cfg.api_secret,
        cfg.api_timeout_secs,
    )?);

    setup_account(&rest, &cfg).await?;
    let filters = rest.symbol_filters(&cfg.symbol).await?;
    info!(
        "Symbol filters: tick={}, step={}, price_prec={}, qty_prec={}",
        filters.tick_size, filters.step_size, filters.price_precision, filters.quantity_precision
    );
    let initial_balance = rest.balance_usdt().await.unwrap_or(0.0);

    // --- 4. channels + producers ---
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();

    // Price WebSocket
    {
        let ws_base = cfg.ws_base().to_string();
        let symbol = cfg.symbol.clone();
        let tx = tx.clone();
        tokio::spawn(async move { websocket::run_price_stream(ws_base, symbol, tx).await });
    }
    // User-data WebSocket (order fills)
    {
        let rest = rest.clone();
        let ws_base = cfg.ws_base().to_string();
        let symbol = cfg.symbol.clone();
        let tx = tx.clone();
        tokio::spawn(async move { websocket::run_user_stream(rest, ws_base, symbol, tx).await });
    }
    // Consistency ticker (every 15s, like the Python thread)
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            // Every 5s: reconcile position + clean up leftover orders (e.g. after
            // a manual close, cancel the stranded TP/SL). Was 15s.
            let mut ticker = tokio::time::interval(Duration::from_secs(5));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if tx.send(Event::ConsistencyTick).is_err() {
                    break;
                }
            }
        });
    }
    // Heartbeat ticker (every 60s)
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if tx.send(Event::Heartbeat).is_err() {
                    break;
                }
            }
        });
    }

    // --- 5. single-owner event loop ---
    let mut bot = TradingBot::new(cfg, rest.clone(), filters, initial_balance);
    bot.log_startup();
    info!("Bot is starting... Press Ctrl+C to stop");

    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        bot.handle(event).await;
                        if bot.stop_flag {
                            info!("Stop flag set (take profit) — shutting down");
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received");
                break;
            }
            // systemctl stop/restart sends SIGTERM. Without this the process is
            // killed outright and the open position keeps its orders on the
            // exchange with nothing watching them.
            _ = terminate.recv() => {
                info!("SIGTERM received (systemctl stop/restart)");
                break;
            }
        }
    }

    bot.shutdown().await;
    Ok(())
}

/// Set leverage and CROSSED margin type before trading.
async fn setup_account(rest: &BinanceRest, cfg: &TradingConfig) -> Result<()> {
    rest.change_leverage(&cfg.symbol, cfg.leverage).await?;
    info!("Leverage set to {}x", cfg.leverage);
    match rest.change_margin_type_crossed(&cfg.symbol).await {
        Ok(_) => info!("Margin type set to CROSSED"),
        Err(e) => error!("Could not set margin type: {e}"),
    }
    Ok(())
}

/// Decide the entry mode. Priority:
///   1. `.env` ENTRY_MODE = "custom" -> use CUSTOM_ENTRY_LOW / CUSTOM_ENTRY_HIGH
///   2. `.env` ENTRY_MODE = "auto"   -> watch/confirm
///   3. not set + real terminal      -> interactive prompt (manual runs)
///   4. not set + no terminal (service) -> default to auto (never hang)
fn resolve_entry_mode() -> Result<(Option<f64>, Option<f64>)> {
    let mode = std::env::var("ENTRY_MODE").unwrap_or_default().trim().to_lowercase();
    match mode.as_str() {
        "custom" => {
            let low = env_f64("CUSTOM_ENTRY_LOW");
            let high = env_f64("CUSTOM_ENTRY_HIGH");
            if low.is_none() || high.is_none() {
                return Err(anyhow!(
                    "ENTRY_MODE=custom needs CUSTOM_ENTRY_LOW and CUSTOM_ENTRY_HIGH set in .env"
                ));
            }
            println!("Entry mode: CUSTOM range from .env (LOW={low:?} -> SHORT, HIGH={high:?} -> LONG)");
            Ok((low, high))
        }
        "auto" => {
            println!("Entry mode: AUTO (watch/confirm) from .env");
            Ok((None, None))
        }
        _ => {
            if io::stdin().is_terminal() {
                ask_custom_entry_range()
            } else {
                println!("ENTRY_MODE not set and no terminal — defaulting to AUTO (watch/confirm).");
                Ok((None, None))
            }
        }
    }
}

fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
}

/// Port of `ask_custom_entry_range()` from main.py.
fn ask_custom_entry_range() -> Result<(Option<f64>, Option<f64>)> {
    loop {
        println!("\nChoose entry mode:");
        println!("  1) Custom entry price range");
        println!("  2) Default percentage (watch/confirm)");
        let choice = prompt("Enter 1 or 2: ")?;
        match choice.trim() {
            "2" => return Ok((None, None)),
            "1" => loop {
                let low = ask_float("Enter the LOWER range price (bot goes SHORT if hit): ")?;
                let high = ask_float("Enter the HIGHER range price (bot goes LONG if hit): ")?;
                if low >= high {
                    println!("The lower price ({low}) must be less than the higher price ({high}). Try again.\n");
                    continue;
                }
                return Ok((Some(low), Some(high)));
            },
            _ => println!("Please enter 1 or 2."),
        }
    }
}

fn ask_float(msg: &str) -> Result<f64> {
    loop {
        let raw = prompt(msg)?;
        match raw.trim().parse::<f64>() {
            Ok(v) if v > 0.0 => return Ok(v),
            Ok(_) => println!("Price must be greater than 0. Try again."),
            Err(_) => println!("That's not a valid number. Try again."),
        }
    }
}

fn prompt(msg: &str) -> Result<String> {
    print!("{msg}");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf)
}
