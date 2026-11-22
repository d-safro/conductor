//! Things that live only in memory: a user's devices and the commands between them, phone sign-in codes, and
//! Jams' guests and commands. Long-polls wait on one change counter and re-check what they're waiting for.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::{Map, Value, json};
use tokio::sync::watch;

pub const ONLINE_SECS: i64 = 90;
pub const PAIR_SECS: i64 = 300;
pub const MAX_PAIRS: usize = 30;
/// Sign-in codes waiting from one address: enough to reopen the dialog a few times, too few to crowd others out.
pub const MAX_PAIRS_PER_IP: usize = 5;
pub const JAM_ONLINE_SECS: i64 = 60;
pub const JAM_GUEST_SECS: i64 = 120;

pub fn now_ms() -> i64 {
    crate::db::now_ms()
}

pub struct Device {
    pub name: String,
    pub kind: String,
    pub client: String,
    pub state: Map<String, Value>,
    pub seen_ms: i64,
    pub cmds: Vec<Value>,
    pub seq: i64,
}

impl Device {
    pub fn public(&self, id: &str, now: i64) -> Value {
        json!({ "id": id, "name": self.name, "kind": self.kind, "client": self.client,
                "online": now - self.seen_ms < ONLINE_SECS * 1000, "seen": self.seen_ms, "state": self.state })
    }
    pub fn state_at(&self) -> i64 {
        self.state.get("at").and_then(Value::as_i64).unwrap_or(0)
    }
}

pub struct Pair {
    pub code: String,
    /// who asked for it (jam::client_ip)
    pub ip: String,
    pub created_ms: i64,
    pub name: String,
    pub login: Option<Value>,
}

#[derive(Default)]
pub struct Jam {
    pub token: String,
    pub owner_id: String,
    pub host_name: String,
    pub seen_ms: i64,
    pub state: Map<String, Value>,
    pub cmds: Vec<Value>,
    pub seq: i64,
    pub covers: std::collections::HashSet<String>,
    pub guests: HashMap<String, (String, i64)>,
    pub hits: HashMap<String, Vec<i64>>,
    pub last_skip_ms: i64,
}

impl Jam {
    pub fn push(&mut self, mut cmd: Value) {
        self.seq += 1;
        cmd["seq"] = self.seq.into();
        self.cmds.push(cmd);
        let n = self.cmds.len();
        if n > 300 {
            self.cmds.drain(..n - 300);
        }
    }
    pub fn guest_count(&self, now: i64) -> usize {
        self.guests.values().filter(|(_, t)| now - t < JAM_GUEST_SECS * 1000).count()
    }
    pub fn public(&self, now: i64) -> Value {
        json!({ "host": self.host_name, "online": now - self.seen_ms < JAM_ONLINE_SECS * 1000,
                "guests": self.guest_count(now), "state": self.state, "pending": [] })
    }
}

#[derive(Default)]
pub struct Inner {
    /// (user id, device id) → device
    pub devices: HashMap<(String, String), Device>,
    /// sign-in token → code waiting for a phone
    pub pairs: HashMap<String, Pair>,
    /// Jam token → Jam
    pub jams: HashMap<String, Jam>,
}

pub struct Live {
    pub inner: Mutex<Inner>,
    tick: watch::Sender<u64>,
}

impl Default for Live {
    fn default() -> Self {
        Self { inner: Mutex::default(), tick: watch::channel(0).0 }
    }
}

impl Live {
    /// Something changed: wake every long-poll so it can look again.
    pub fn bump(&self) {
        self.tick.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Call `check` now and after every change until it returns Some, or `secs` have passed (then it's called once
    /// more with `last = true`, and must answer).
    pub async fn wait<T>(&self, secs: u64, mut check: impl FnMut(&mut Inner, bool) -> Option<T>) -> T {
        let mut rx = self.tick.subscribe();
        let end = Instant::now() + Duration::from_secs(secs.min(25));
        loop {
            let last = Instant::now() >= end;
            if let Some(x) = check(&mut self.lock(), last) {
                return x;
            }
            if last {
                // check() must answer on the last call; keep looping defensively if it didn't
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            let _ = tokio::time::timeout_at(end.into(), rx.changed()).await;
        }
    }
}
