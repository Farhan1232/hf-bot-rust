//! Throwaway check: does the aggTrade (last traded price) stream actually
//! deliver on the URL the bot uses, and how far is the mark price from it?
//! Run with:  cargo run --release --example stream_check -- BANKUSDT

use futures_util::StreamExt;
use serde_json::Value;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

async fn sample(url: String, field: &str, label: &str) -> Option<f64> {
    let (mut ws, _) = connect_async(&url).await.ok()?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout_at(deadline, ws.next()).await {
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            if let Some(p) = v.get(field).and_then(|x| x.as_str()).and_then(|s| s.parse().ok()) {
                println!("{label}: {p}");
                return Some(p);
            }
        }
    }
    println!("{label}: NO DATA");
    None
}

#[tokio::main]
async fn main() {
    let symbol = std::env::args().nth(1).unwrap_or_else(|| "BANKUSDT".into());
    let s = symbol.to_lowercase();
    let base = "wss://fstream.binance.com";
    let last = sample(format!("{base}/market/ws/{s}@aggTrade"), "p", "last traded").await;
    let mark = sample(format!("{base}/market/ws/{s}@markPrice@1s"), "p", "mark").await;
    if let (Some(l), Some(m)) = (last, mark) {
        println!("gap: {:.4}% of last", (m - l).abs() / l * 100.0);
    }
}
