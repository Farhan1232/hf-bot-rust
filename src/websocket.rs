//! Price and user-data WebSocket streams.
//!
//! Mirrors `websocket_manager.py`, including the post-2026-04-23 Binance URL
//! split: mark price lives under `/market/ws/...` and user-data (listenKey)
//! lives under `/private/ws/...`. Both streams auto-reconnect after a 5s
//! backoff, like the Python version.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::events::{Event, OrderUpdate};
use crate::rest::BinanceRest;

/// Mark-price stream: `<symbol>@markPrice`. Each message carries the mark
/// price in field `p`.
pub async fn run_price_stream(ws_base: String, symbol: String, tx: UnboundedSender<Event>) {
    // @markPrice@1s = mark price every 1 second (the plain @markPrice is every
    // ~3s). Faster updates make the bot-side software stop react sooner when it
    // is used as a fallback.
    let stream = format!("{}@markPrice@1s", symbol.to_lowercase());
    let url = format!("{ws_base}/market/ws/{stream}");

    loop {
        match connect_async(&url).await {
            Ok((mut ws, _)) => {
                info!("Price WebSocket connected for {symbol}");
                while let Some(msg) = ws.next().await {
                    match msg {
                        Ok(Message::Text(txt)) => {
                            if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                                if let Some(p) = v
                                    .get("p")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                                {
                                    if tx.send(Event::Price(p)).is_err() {
                                        return; // bot loop gone; stop the task
                                    }
                                }
                            }
                        }
                        Ok(Message::Close(_)) => break,
                        Err(e) => {
                            error!("Price WebSocket error: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => error!("Price WebSocket connect error: {e}"),
        }
        if tx.is_closed() {
            return;
        }
        warn!("Price WebSocket closed — reconnecting in 5s");
        sleep(Duration::from_secs(5)).await;
    }
}

/// Last-traded-price stream: `<symbol>@aggTrade`. Each message carries the
/// price of an actual trade in field `p`.
///
/// This runs ALONGSIDE the mark-price stream on purpose. Mark price is an
/// index-derived number Binance publishes for funding and liquidation; on a
/// thin symbol it can sit 0.5-0.8% away from where trades are printing. Entry
/// triggers and the bot-side stop must follow the price the orders actually
/// fill at, or a 0.1% stop gets measured against a number that is already
/// 0.5% out — which is exactly how a 0.1% stop turned into 0.62%.
pub async fn run_last_price_stream(ws_base: String, symbol: String, tx: UnboundedSender<Event>) {
    let stream = format!("{}@aggTrade", symbol.to_lowercase());
    let url = format!("{ws_base}/market/ws/{stream}");

    loop {
        match connect_async(&url).await {
            Ok((mut ws, _)) => {
                info!("Last-price (aggTrade) WebSocket connected for {symbol}");
                while let Some(msg) = ws.next().await {
                    match msg {
                        Ok(Message::Text(txt)) => {
                            if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                                if let Some(p) = v
                                    .get("p")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                                {
                                    if tx.send(Event::LastPrice(p)).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                        Ok(Message::Close(_)) => break,
                        Err(e) => {
                            error!("Last-price WebSocket error: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => error!("Last-price WebSocket connect error: {e}"),
        }
        if tx.is_closed() {
            return;
        }
        warn!("Last-price WebSocket closed — reconnecting in 5s");
        sleep(Duration::from_secs(5)).await;
    }
}

/// User-data stream. Creates a fresh listenKey on every (re)connect and keeps
/// it alive with a PUT every 30 minutes while connected. Forwards FILLED
/// `ORDER_TRADE_UPDATE` events to the bot loop.
pub async fn run_user_stream(
    rest: Arc<BinanceRest>,
    ws_base: String,
    symbol: String,
    tx: UnboundedSender<Event>,
) {
    loop {
        let listen_key = match rest.create_listen_key().await {
            Ok(k) => k,
            Err(e) => {
                error!("Could not obtain listen key: {e}");
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        info!("Obtained listen key: {}...", short(&listen_key));
        let url = format!("{ws_base}/private/ws/{listen_key}");

        match connect_async(&url).await {
            Ok((mut ws, _)) => {
                info!("User data stream connected");
                let mut keepalive = tokio::time::interval(Duration::from_secs(30 * 60));
                keepalive.tick().await; // consume the immediate first tick

                loop {
                    tokio::select! {
                        maybe_msg = ws.next() => {
                            match maybe_msg {
                                Some(Ok(Message::Text(txt))) => {
                                    handle_user_message(&txt, &symbol, &tx);
                                    if tx.is_closed() { return; }
                                }
                                Some(Ok(Message::Close(_))) | None => break,
                                Some(Err(e)) => { error!("User stream error: {e}"); break; }
                                _ => {}
                            }
                        }
                        _ = keepalive.tick() => {
                            if let Err(e) = rest.keepalive_listen_key().await {
                                warn!("listenKey keepalive failed: {e}");
                            }
                        }
                    }
                }
            }
            Err(e) => error!("User stream connect error: {e}"),
        }
        if tx.is_closed() {
            return;
        }
        warn!("User data stream closed — reconnecting in 5s");
        sleep(Duration::from_secs(5)).await;
    }
}

fn handle_user_message(txt: &str, symbol: &str, tx: &UnboundedSender<Event>) {
    let v: Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return,
    };
    if v.get("e").and_then(|e| e.as_str()) != Some("ORDER_TRADE_UPDATE") {
        return;
    }
    let Some(o) = v.get("o") else { return };
    // Only forward filled orders for our symbol (bot ignores the rest anyway).
    if o.get("X").and_then(|x| x.as_str()) != Some("FILLED") {
        return;
    }
    if o.get("s").and_then(|s| s.as_str()) != Some(symbol) {
        return;
    }
    let _ = tx.send(Event::OrderFill(OrderUpdate::from_json(o)));
}

fn short(key: &str) -> &str {
    &key[..key.len().min(10)]
}
