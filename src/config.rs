//! Configuration: loads settings from the external `.env` file (via `dotenvy`)
//! exactly like the Python version loaded them with `python-dotenv`.
//!
//! IMPORTANT (client requirement #7): API keys are read from `.env` at
//! runtime. They are NEVER hardcoded into the compiled binary, so the keys
//! can be rotated by simply editing `.env` — no recompilation needed.

use anyhow::{anyhow, Result};
use std::env;

// --- Binance endpoint bases (same hosts the working Python bot used) ---
pub const LIVE_FAPI_BASE: &str = "https://fapi.binance.com";
pub const DEMO_FAPI_BASE: &str = "https://demo-fapi.binance.com";
pub const LIVE_WS_BASE: &str = "wss://fstream.binance.com";
pub const DEMO_WS_BASE: &str = "wss://demo-fstream.binance.com";
// Binance Futures TESTNET — free API keys with NO KYC (log in with GitHub/
// Google at https://testnet.binancefuture.com). Verified to serve the same
// `/market/ws/` and `/private/ws/` WebSocket paths this bot uses.
pub const TESTNET_FAPI_BASE: &str = "https://testnet.binancefuture.com";
pub const TESTNET_WS_BASE: &str = "wss://fstream.binancefuture.com";

#[derive(Clone, Debug)]
pub struct TradingConfig {
    pub symbol: String,
    pub leverage: u32,
    pub take_profit_percentage: f64,
    pub position_size: f64,
    pub api_key: String,
    pub api_secret: String,
    pub use_demo: bool,
    /// When true, overrides `use_demo` and points at Binance Futures Testnet
    /// (free, no-KYC keys). Used for testing before delivery.
    pub use_testnet: bool,
    /// Kept for parity with the Python config; the WebSocket push model means
    /// the bot reacts to every tick rather than polling on an interval.
    #[allow(dead_code)]
    pub price_check_interval: f64,
    pub watch_percentage: f64,
    pub confirm_percentage: f64,
    pub stop_loss_percentage: f64,
    pub api_timeout_secs: u64,
    pub custom_entry_price_low: Option<f64>,
    pub custom_entry_price_high: Option<f64>,
}

impl TradingConfig {
    pub fn fapi_base(&self) -> &'static str {
        if self.use_testnet {
            TESTNET_FAPI_BASE
        } else if self.use_demo {
            DEMO_FAPI_BASE
        } else {
            LIVE_FAPI_BASE
        }
    }

    pub fn ws_base(&self) -> &'static str {
        if self.use_testnet {
            TESTNET_WS_BASE
        } else if self.use_demo {
            DEMO_WS_BASE
        } else {
            LIVE_WS_BASE
        }
    }

    /// Human-readable environment name for logs.
    pub fn env_name(&self) -> &'static str {
        if self.use_testnet {
            "TESTNET"
        } else if self.use_demo {
            "DEMO"
        } else {
            "LIVE"
        }
    }

    /// Build the config from `.env`, then validate it. `custom_entry_*` come
    /// from the interactive menu in `main`, not from `.env`.
    pub fn load(
        custom_entry_price_low: Option<f64>,
        custom_entry_price_high: Option<f64>,
    ) -> Result<Self> {
        // Load .env sitting next to the binary / in the working dir.
        let _ = dotenvy::dotenv();

        let cfg = TradingConfig {
            symbol: env_str("SYMBOL", "XRPUSDT"),
            leverage: env_parse("LEVERAGE", 15),
            take_profit_percentage: env_parse("TAKE_PROFIT_PERCENTAGE", 2.0),
            position_size: env_parse("POSITION_SIZE", 10.0),
            api_key: env_str("BINANCE_API_KEY", ""),
            api_secret: env_str("BINANCE_API_SECRET", ""),
            use_demo: env_bool("USE_DEMO", true),
            use_testnet: env_bool("TESTNET", false),
            price_check_interval: env_parse("PRICE_CHECK_INTERVAL", 0.1),
            watch_percentage: env_parse("WATCH_PERCENTAGE", 0.05),
            confirm_percentage: env_parse("CONFIRM_PERCENTAGE", 0.10),
            stop_loss_percentage: env_parse("STOP_LOSS_PERCENTAGE", 0.05),
            api_timeout_secs: env_parse("API_TIMEOUT", 30),
            custom_entry_price_low,
            custom_entry_price_high,
        };

        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        // --- Credentials (client requirement: clear error if keys missing) ---
        let mut missing = Vec::new();
        if self.api_key.is_empty() || self.api_key == "your_api_key_here" {
            missing.push("BINANCE_API_KEY");
        }
        if self.api_secret.is_empty() || self.api_secret == "your_api_secret_here" {
            missing.push("BINANCE_API_SECRET");
        }
        if !missing.is_empty() {
            return Err(anyhow!(
                "Missing in .env: {}. Copy .env.example to .env and add your keys.",
                missing.join(", ")
            ));
        }

        // --- Custom entry range must be set as a pair and be ordered ---
        match (self.custom_entry_price_low, self.custom_entry_price_high) {
            (Some(low), Some(high)) if low >= high => {
                return Err(anyhow!(
                    "custom_entry_price_low ({low}) must be less than custom_entry_price_high ({high})."
                ));
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(anyhow!(
                    "custom_entry_price_low and custom_entry_price_high must both be set together, or both be None."
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

fn env_str(key: &str, default: &str) -> String {
    env::var(key)
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|_| default.to_string())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(v) => matches!(
            v.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    match env::var(key) {
        Ok(v) => v.trim().parse().unwrap_or(default),
        Err(_) => default,
    }
}
