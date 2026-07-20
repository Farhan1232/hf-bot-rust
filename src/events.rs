//! Events flowing into the single-owner bot loop.
//!
//! The Python bot used threads + a `position_operation_in_progress` lock to
//! stop the price callback and the order-fill callback from stepping on each
//! other. In Rust we get that mutual exclusion for free: every producer
//! (price WS, user-data WS, timers) sends an `Event` into ONE channel, and the
//! bot processes them one at a time in a single task. No locks, no deadlocks,
//! no 30s lock-timeout watchdog needed.

use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Event {
    /// New mark price from the price WebSocket.
    Price(f64),
    /// A FILLED order event from the user-data WebSocket.
    OrderFill(OrderUpdate),
    /// Periodic reconciliation of tracked vs. actual position + orphan cleanup.
    ConsistencyTick,
    /// Periodic status log line.
    Heartbeat,
}

/// The subset of Binance's `ORDER_TRADE_UPDATE` `o` object the bot reacts to.
#[derive(Debug, Clone)]
pub struct OrderUpdate {
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub status: String,
    pub reduce_only: bool,
}

impl OrderUpdate {
    pub fn from_json(o: &Value) -> Self {
        OrderUpdate {
            symbol: str_field(o, "s"),
            side: str_field(o, "S"),
            order_type: str_field(o, "o"),
            status: str_field(o, "X"),
            reduce_only: bool_field(o, "R"),
        }
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn bool_field(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.to_lowercase().as_str(), "true" | "1" | "yes"),
        _ => false,
    }
}
