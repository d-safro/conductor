//! Deezer's public API (no account): the catalog of songs you don't have, their covers and 30-second previews.
//! Answers are kept in memory for 6 hours; a call that failed (no answer, an error page) for a minute.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::Value;

const TTL: Duration = Duration::from_secs(6 * 3600);
const FAILED_TTL: Duration = Duration::from_secs(60);

pub struct Deezer {
    base: String,
    http: reqwest::Client,
    /// url → (when, the answer; None: the call failed)
    cache: Mutex<HashMap<String, (Instant, Option<Value>)>>,
}

impl Deezer {
    /// `proxy`: Conductor's download traffic goes through it when set (see net.rs).
    pub fn new(base: &str, proxy: Option<&str>) -> Self {
        let b = reqwest::Client::builder().user_agent(concat!("Conductor/", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(10));
        let http = crate::net::with_proxy(b, proxy).build().expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), http, cache: Mutex::default() }
    }

    fn cached(&self, url: &str) -> Option<Option<Value>> {
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.get(url).filter(|(t, v)| t.elapsed() < if v.is_some() { TTL } else { FAILED_TTL }).map(|(_, v)| v.clone())
    }

    /// GET <path>?<query> → the JSON, or an empty object when Deezer has nothing / doesn't answer.
    pub async fn get(&self, path: &str, query: &[(&str, &str)]) -> Value {
        self.try_get(path, query).await.unwrap_or_else(|| Value::Object(Default::default()))
    }

    /// `get`, telling a failed call (None) from Deezer having nothing (an empty object).
    pub async fn try_get(&self, path: &str, query: &[(&str, &str)]) -> Option<Value> {
        let mut url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        if !query.is_empty() {
            url.push('?');
            url.push_str(&form_urlencoded::Serializer::new(String::new()).extend_pairs(query).finish());
        }
        if let Some(v) = self.cached(&url) {
            return v;
        }
        let mut data = None;
        for _ in 0..3 {
            match self.http.get(&url).send().await {
                Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                    Ok(v) if v.get("error").is_some() => {
                        if v["error"]["code"] == 4 {
                            tokio::time::sleep(Duration::from_millis(1500)).await; // too many requests: wait a moment
                            continue;
                        }
                        data = Some(Value::Object(Default::default())); // Deezer's own answer: it has nothing
                        break;
                    }
                    Ok(v) => {
                        data = Some(v);
                        break;
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                },
                _ => tokio::time::sleep(Duration::from_millis(500)).await,
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
        self.try_list(path, query).await.unwrap_or_default()
    }

    /// `list`, None when the call failed.
    pub async fn try_list(&self, path: &str, query: &[(&str, &str)]) -> Option<Vec<Value>> {
        Some(self.try_get(path, query).await?.get("data").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_calls_are_kept_only_a_minute() {
        let dz = Deezer::new("http://deezer.invalid", None);
        let long_ago = Instant::now().checked_sub(Duration::from_secs(61)).unwrap();
        let mut c = dz.cache.lock().unwrap();
        c.insert("a".into(), (long_ago, None));
        c.insert("b".into(), (long_ago, Some(Value::Null)));
        c.insert("c".into(), (Instant::now(), None));
        drop(c);
        assert_eq!(dz.cached("a"), None, "failed a minute ago: asked again");
        assert_eq!(dz.cached("b"), Some(Some(Value::Null)), "an answer is kept");
        assert_eq!(dz.cached("c"), Some(None));
    }
}
