//! Deezer's public API (no account): the catalog of songs you don't have, their covers and 30-second previews.
//! Answers are kept in memory for 6 hours.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::Value;

const TTL: Duration = Duration::from_secs(6 * 3600);

pub struct Deezer {
    base: String,
    http: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Deezer {
    /// `proxy`: Conductor's download traffic goes through it when set (see net.rs).
    pub fn new(base: &str, proxy: Option<&str>) -> Self {
        let b = reqwest::Client::builder().user_agent(concat!("Conductor/", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(10));
        let http = crate::net::with_proxy(b, proxy).build().expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), http, cache: Mutex::default() }
    }

    fn cached(&self, url: &str) -> Option<Value> {
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.get(url).filter(|(t, _)| t.elapsed() < TTL).map(|(_, v)| v.clone())
    }

    /// GET <path>?<query> → the JSON, or an empty object when Deezer has nothing / doesn't answer.
    pub async fn get(&self, path: &str, query: &[(&str, &str)]) -> Value {
        let mut url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        if !query.is_empty() {
            url.push('?');
            url.push_str(&form_urlencoded::Serializer::new(String::new()).extend_pairs(query).finish());
        }
        if let Some(v) = self.cached(&url) {
            return v;
        }
        let mut data = Value::Object(Default::default());
        for _ in 0..3 {
            match self.http.get(&url).send().await {
                Ok(r) => match r.json::<Value>().await {
                    Ok(v) if v.get("error").is_some() => {
                        if v["error"]["code"] == 4 {
                            tokio::time::sleep(Duration::from_millis(1500)).await; // too many requests: wait a moment
                            continue;
                        }
                        break;
                    }
                    Ok(v) => {
                        data = v;
                        break;
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                },
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.len() > 4000 {
            c.retain(|_, (t, _)| t.elapsed() < TTL / 2);
        }
        c.insert(url, (Instant::now(), data.clone()));
        data
    }

    /// The "data" list of a list answer.
    pub async fn list(&self, path: &str, query: &[(&str, &str)]) -> Vec<Value> {
        self.get(path, query).await.get("data").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}
