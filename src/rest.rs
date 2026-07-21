//! Binance USDⓈ-M Futures REST client.
//!
//! Replaces `python-binance`. Signed requests use HMAC-SHA256 over the query
//! string, exactly like Binance requires.
//!
//! SPEED / client requirement #2: there is NO `futures_time()` round-trip
//! before each call. The Python bot re-synced the clock with Binance before
//! every signed request (`call_with_time_sync` -> `sync_binance_time`), which
//! added a full network round-trip to every order. On a properly-timed server
//! (AWS Tokyo) that is unnecessary — we simply stamp requests with the local
//! clock and send a generous `recvWindow`.

use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// A structured Binance API error so callers can branch on the numeric code
/// (e.g. -2021 "order would immediately trigger", -4046 "no need to change
/// margin type").
#[derive(thiserror::Error, Debug)]
#[error("Binance API error {code}: {msg}")]
pub struct ApiError {
    pub code: i64,
    pub msg: String,
}

/// Precision/filter info for the traded symbol, read once at startup.
#[derive(Debug, Clone)]
pub struct SymbolFilters {
    pub tick_size: f64,
    pub step_size: f64,
    pub price_precision: usize,
    pub quantity_precision: usize,
}

pub struct BinanceRest {
    http: reqwest::Client,
    base: String,
    api_key: String,
    api_secret: String,
    recv_window: u64,
}

impl BinanceRest {
    pub fn new(base: &str, api_key: &str, api_secret: &str, timeout_secs: u64) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()?;
        Ok(BinanceRest {
            http,
            base: base.to_string(),
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            recv_window: 60_000,
        })
    }

    // ----- low level -------------------------------------------------------

    fn sign(&self, query: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(self.api_secret.as_bytes()).expect("HMAC accepts any key size");
        mac.update(query.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    async fn parse(resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        let text = resp.text().await?;
        let val: Value =
            serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()));

        if !status.is_success() {
            if let Some(code) = val.get("code").and_then(|c| c.as_i64()) {
                let msg = val
                    .get("msg")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                return Err(ApiError { code, msg }.into());
            }
            return Err(anyhow!("HTTP {}: {}", status, text));
        }
        Ok(val)
    }

    /// Signed request. All params go in the query string (Binance accepts this
    /// for GET/POST/DELETE), so the signature covers exactly what is sent.
    async fn signed(
        &self,
        method: reqwest::Method,
        path: &str,
        mut params: Vec<(&str, String)>,
    ) -> Result<Value> {
        params.push(("recvWindow", self.recv_window.to_string()));
        params.push(("timestamp", now_ms().to_string()));
        let query = serde_urlencoded::to_string(&params)?;
        let signature = self.sign(&query);
        let url = format!("{}{}?{}&signature={}", self.base, path, query, signature);
        let resp = self
            .http
            .request(method, &url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await?;
        Self::parse(resp).await
    }

    async fn public(&self, path: &str, params: Vec<(&str, String)>) -> Result<Value> {
        let query = serde_urlencoded::to_string(&params)?;
        let url = if query.is_empty() {
            format!("{}{}", self.base, path)
        } else {
            format!("{}{}?{}", self.base, path, query)
        };
        let resp = self.http.get(&url).send().await?;
        Self::parse(resp).await
    }

    // ----- account / setup -------------------------------------------------

    pub async fn change_leverage(&self, symbol: &str, leverage: u32) -> Result<Value> {
        self.signed(
            reqwest::Method::POST,
            "/fapi/v1/leverage",
            vec![("symbol", symbol.into()), ("leverage", leverage.to_string())],
        )
        .await
    }

    /// Set margin type to CROSSED. -4046 ("no need to change") is treated as OK.
    pub async fn change_margin_type_crossed(&self, symbol: &str) -> Result<()> {
        let res = self
            .signed(
                reqwest::Method::POST,
                "/fapi/v1/marginType",
                vec![("symbol", symbol.into()), ("marginType", "CROSSED".into())],
            )
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) => {
                if let Some(api) = e.downcast_ref::<ApiError>() {
                    if api.code == -4046 {
                        return Ok(());
                    }
                }
                Err(e)
            }
        }
    }

    /// Read tick size / step size / precisions for the symbol.
    pub async fn symbol_filters(&self, symbol: &str) -> Result<SymbolFilters> {
        let info = self.public("/fapi/v1/exchangeInfo", vec![]).await?;
        let symbols = info
            .get("symbols")
            .and_then(|s| s.as_array())
            .ok_or_else(|| anyhow!("exchangeInfo: no symbols array"))?;
        let sym = symbols
            .iter()
            .find(|s| s.get("symbol").and_then(|x| x.as_str()) == Some(symbol))
            .ok_or_else(|| anyhow!("symbol {symbol} not found in exchangeInfo"))?;
        let filters = sym
            .get("filters")
            .and_then(|f| f.as_array())
            .ok_or_else(|| anyhow!("no filters for {symbol}"))?;

        let tick_size = filter_field(filters, "PRICE_FILTER", "tickSize")?;
        let step_size = filter_field(filters, "LOT_SIZE", "stepSize")?;

        Ok(SymbolFilters {
            tick_size,
            step_size,
            price_precision: precision_from(tick_size),
            quantity_precision: precision_from(step_size),
        })
    }

    pub async fn balance_usdt(&self) -> Result<f64> {
        let arr = self
            .signed(reqwest::Method::GET, "/fapi/v2/balance", vec![])
            .await?;
        if let Some(items) = arr.as_array() {
            for a in items {
                if a.get("asset").and_then(|x| x.as_str()) == Some("USDT") {
                    return Ok(get_f64(a, "balance"));
                }
            }
        }
        Ok(0.0)
    }

    pub async fn symbol_price(&self, symbol: &str) -> Result<f64> {
        let v = self
            .public("/fapi/v1/ticker/price", vec![("symbol", symbol.into())])
            .await?;
        Ok(get_f64(&v, "price"))
    }

    /// Returns raw position objects for the symbol (`/fapi/v2/positionRisk`).
    pub async fn position_information(&self, symbol: &str) -> Result<Vec<Value>> {
        let v = self
            .signed(
                reqwest::Method::GET,
                "/fapi/v2/positionRisk",
                vec![("symbol", symbol.into())],
            )
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    // ----- orders ----------------------------------------------------------

    pub async fn create_market_order(
        &self,
        symbol: &str,
        side: &str,
        quantity: &str,
        reduce_only: bool,
    ) -> Result<Value> {
        let mut params = vec![
            ("symbol", symbol.into()),
            ("side", side.into()),
            ("type", "MARKET".into()),
            ("quantity", quantity.into()),
        ];
        if reduce_only {
            params.push(("reduceOnly", "true".into()));
        }
        self.signed(reqwest::Method::POST, "/fapi/v1/order", params).await
    }

    pub async fn create_limit_order(
        &self,
        symbol: &str,
        side: &str,
        price: &str,
        quantity: &str,
        reduce_only: bool,
    ) -> Result<Value> {
        let mut params = vec![
            ("symbol", symbol.into()),
            ("side", side.into()),
            ("type", "LIMIT".into()),
            ("timeInForce", "GTC".into()),
            ("price", price.into()),
            ("quantity", quantity.into()),
        ];
        if reduce_only {
            params.push(("reduceOnly", "true".into()));
        }
        self.signed(reqwest::Method::POST, "/fapi/v1/order", params).await
    }

    /// Place a stop-loss as a CONDITIONAL algo order via the new endpoint
    /// (`POST /fapi/v1/algoOrder`). Since Binance's 2025-12-09 migration, stop
    /// orders MUST go here — the old `/fapi/v1/order` rejects them with -4120.
    /// Returns the `algoId` so it can be cancelled later.
    pub async fn create_algo_stop_market(
        &self,
        symbol: &str,
        side: &str,
        trigger_price: &str,
        quantity: &str,
        reduce_only: bool,
    ) -> Result<i64> {
        let mut params = vec![
            ("algoType", "CONDITIONAL".into()),
            ("symbol", symbol.into()),
            ("side", side.into()),
            ("type", "STOP_MARKET".into()),
            ("triggerPrice", trigger_price.into()),
            ("quantity", quantity.into()),
            // Trigger off the LAST traded price, the same price the entry was
            // filled at and the same one shown in the UI. Left unset, Binance
            // may evaluate the trigger against the MARK price, which on thin
            // symbols can sit ~0.5% away from last — that makes every stop
            // tighter than the gap look "already triggered" (-2021).
            ("workingType", "CONTRACT_PRICE".into()),
        ];
        if reduce_only {
            params.push(("reduceOnly", "true".into()));
        }
        let v = self
            .signed(reqwest::Method::POST, "/fapi/v1/algoOrder", params)
            .await?;
        get_i64(&v, "algoId").ok_or_else(|| anyhow!("no algoId in algoOrder response: {v}"))
    }

    /// List open CONDITIONAL algo orders (the resting stop-losses) for a symbol.
    pub async fn open_algo_orders(&self, symbol: &str) -> Result<Vec<Value>> {
        let v = self
            .signed(
                reqwest::Method::GET,
                "/fapi/v1/openAlgoOrders",
                vec![("symbol", symbol.into())],
            )
            .await?;
        // Response may be a bare array or an object with an "orders"/"data" array.
        if let Some(arr) = v.as_array() {
            return Ok(arr.clone());
        }
        for key in ["orders", "data", "algoOrders"] {
            if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
                return Ok(arr.clone());
            }
        }
        Ok(vec![])
    }

    /// Cancel a CONDITIONAL algo order (stop-loss) by its algoId.
    pub async fn cancel_algo_order(&self, algo_id: i64) -> Result<()> {
        self.signed(
            reqwest::Method::DELETE,
            "/fapi/v1/algoOrder",
            vec![("algoId", algo_id.to_string())],
        )
        .await?;
        Ok(())
    }

    /// Regular open orders (LIMIT TP is returned here on USDⓈ-M futures; the
    /// stop-loss is now a separate ALGO order, see create_algo_stop_market).
    pub async fn open_orders(&self, symbol: &str) -> Result<Vec<Value>> {
        let v = self
            .signed(
                reqwest::Method::GET,
                "/fapi/v1/openOrders",
                vec![("symbol", symbol.into())],
            )
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn cancel_all_open_orders(&self, symbol: &str) -> Result<Value> {
        self.signed(
            reqwest::Method::DELETE,
            "/fapi/v1/allOpenOrders",
            vec![("symbol", symbol.into())],
        )
        .await
    }

    // ----- user data stream (listen key) -----------------------------------

    pub async fn create_listen_key(&self) -> Result<String> {
        let v = self
            .signed_no_query(reqwest::Method::POST, "/fapi/v1/listenKey")
            .await?;
        v.get("listenKey")
            .and_then(|k| k.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("no listenKey in response: {v}"))
    }

    pub async fn keepalive_listen_key(&self) -> Result<()> {
        self.signed_no_query(reqwest::Method::PUT, "/fapi/v1/listenKey")
            .await?;
        Ok(())
    }

    pub async fn close_listen_key(&self) -> Result<()> {
        self.signed_no_query(reqwest::Method::DELETE, "/fapi/v1/listenKey")
            .await?;
        Ok(())
    }

    /// listenKey endpoints need only the API-key header, no signature/params.
    async fn signed_no_query(&self, method: reqwest::Method, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .request(method, &url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await?;
        Self::parse(resp).await
    }
}

// --- helpers ---------------------------------------------------------------

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_millis()
}

/// Read an integer field that Binance may return as a JSON number or string.
pub fn get_i64(v: &Value, key: &str) -> Option<i64> {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
}

/// Read a numeric field that Binance may return either as a JSON string or a
/// JSON number.
pub fn get_f64(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn filter_field(filters: &[Value], filter_type: &str, field: &str) -> Result<f64> {
    let f = filters
        .iter()
        .find(|f| f.get("filterType").and_then(|x| x.as_str()) == Some(filter_type))
        .ok_or_else(|| anyhow!("missing filter {filter_type}"))?;
    Ok(get_f64(f, field))
}

/// Decimal places implied by a tick/step size, e.g. 0.001 -> 3.
fn precision_from(size: f64) -> usize {
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
