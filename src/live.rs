//! Things that live only in memory: a user's devices and the commands between them, phone sign-in codes, and
//! Jams' guests and commands. Long-polls wait on one change counter and re-check what they're waiting for.
//!
//! The same counter wakes the event streams (`encore.events`, `crate::api::events`): a change that an app may
//! want to hear about at once is also written to a short log of events ([`Live::publish`]), each with who may see
//! it, and every stream reads what's new in the log for its own user when the counter moves.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::{Map, Value, json};
use tokio::sync::watch;

pub const ONLINE_SECS: i64 = 90;
pub const PAIR_SECS: i64 = 300;
pub const MAX_PAIRS: usize = 30;
/// Sign-in codes waiting from one address: enough to reopen the dialog a few times, too few to crowd others out.
pub const MAX_PAIRS_PER_IP: usize = 5;
/// Devices one account keeps (a new one past this drops the one heard from longest ago), and the commands waiting
/// for one device, in bytes.
pub const MAX_DEVICES: usize = 20;
pub const MAX_CMD_BYTES: usize = 512 * 1024;
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

/// Events kept for streams that reconnect (`Last-Event-ID`): older than these, a stream is told to resync.
pub const EVENT_LOG: usize = 512;
/// Event streams one account keeps open (each device has one), and all accounts together.
pub const MAX_STREAMS: usize = 20;
pub const MAX_ALL_STREAMS: usize = 2000;
/// A keep-alive comment this often, so proxies (Caddy's default, nginx's 60 s) keep a quiet stream open.
pub const KEEPALIVE: Duration = Duration::from_secs(20);

/// Who may see an event.
#[derive(Clone, Debug, PartialEq)]
pub enum Audience {
    /// one account (its devices, commands, sync, playlists, Jam)
    User(String),
    /// every account (what getNowPlaying and getScanStatus show everyone)
    Everyone,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: u64,
    pub kind: &'static str,
    pub data: Value,
    pub to: Audience,
    /// only for the stream of this device (and streams that didn't say which device they are): commands
    pub device: Option<String>,
}

impl Event {
    fn for_stream(&self, user: &str, device: Option<&str>) -> bool {
        let who = match &self.to {
            Audience::Everyone => true,
            Audience::User(u) => u == user,
        };
        who && match (&self.device, device) {
            (Some(target), Some(me)) => target == me,
            _ => true,
        }
    }
}

#[derive(Default)]
struct EventLog {
    last: u64,
    events: VecDeque<Event>,
}

/// What a stream reads from the log: what's new for it, or that it fell behind what the log keeps.
pub enum Since {
    New { events: Vec<Event>, last: u64 },
    TooOld { last: u64 },
}

struct OpenStream {
    id: u64,
    user: String,
    device: Option<String>,
    close: Arc<tokio::sync::Notify>,
}

/// An open event stream: counted while it's alive, and told when to stop (replaced by a newer stream from the
/// same device, or the server is stopping).
pub struct StreamTicket {
    live: Arc<Mutex<Vec<OpenStream>>>,
    id: u64,
    close: Arc<tokio::sync::Notify>,
}

impl StreamTicket {
    /// Resolves when this stream is to stop.
    pub async fn closed(&self) {
        self.close.notified().await
    }
}

impl Drop for StreamTicket {
    fn drop(&mut self) {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| s.id != self.id);
    }
}

pub struct Live {
    pub inner: Mutex<Inner>,
    tick: watch::Sender<u64>,
    log: Mutex<EventLog>,
    streams: Arc<Mutex<Vec<OpenStream>>>,
    stream_ids: AtomicU64,
    /// this run of the server, in event ids: an id from an earlier run can't be resumed
    boot: String,
    keepalive_ms: AtomicU64,
    /// the scan watcher (`crate::api::events`) is running
    pub scan_watch: AtomicBool,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            tick: watch::channel(0).0,
            log: Mutex::default(),
            streams: Arc::default(),
            stream_ids: AtomicU64::new(0),
            boot: crate::db::random_id()[..8].to_string(),
            keepalive_ms: AtomicU64::new(KEEPALIVE.as_millis() as u64),
            scan_watch: AtomicBool::new(false),
        }
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

    /// The change counter, for waiting on any change (the event streams).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.tick.subscribe()
    }

    fn log(&self) -> std::sync::MutexGuard<'_, EventLog> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Something changed that `to` may want to hear about at once: written to the event log, and every wait wakes.
    pub fn publish(&self, kind: &'static str, to: Audience, data: Value) {
        self.push(Event { id: 0, kind, data, to, device: None });
    }

    /// A command for one device (`device`) of `user`.
    pub fn publish_command(&self, user: &str, device: &str, data: Value) {
        self.push(Event { id: 0, kind: "command", data, to: Audience::User(user.to_string()), device: Some(device.to_string()) });
    }

    fn push(&self, mut e: Event) {
        {
            let mut log = self.log();
            log.last += 1;
            e.id = log.last;
            log.events.push_back(e);
            while log.events.len() > EVENT_LOG {
                log.events.pop_front();
            }
        }
        self.bump();
    }

    /// A playlist of `owner` changed (or was deleted): its owner's streams hear.
    pub fn playlist_changed(&self, owner: &str, id: &str, deleted: bool) {
        self.publish("playlist", Audience::User(owner.to_string()), json!({ "id": id, "deleted": deleted }));
    }

    /// The newest event's number (0: none yet).
    pub fn last_event(&self) -> u64 {
        self.log().last
    }

    /// The events after `after` that this user's stream (of `device`, if it said) may see.
    pub fn events_since(&self, after: u64, user: &str, device: Option<&str>) -> Since {
        let log = self.log();
        let first = log.events.front().map_or(log.last + 1, |e| e.id);
        if after > log.last || after + 1 < first {
            return Since::TooOld { last: log.last };
        }
        let events = log.events.iter().filter(|e| e.id > after && e.for_stream(user, device)).cloned().collect();
        Since::New { events, last: log.last }
    }

    /// An event number as sent (`id:`), with this run of the server in it.
    pub fn event_id(&self, n: u64) -> String {
        format!("{}-{n}", self.boot)
    }

    /// The number in an event id sent by this run of the server (None: another run's, or not one).
    pub fn parse_event_id(&self, id: &str) -> Option<u64> {
        let (boot, n) = id.trim().split_once('-')?;
        if boot != self.boot {
            return None;
        }
        n.parse().ok()
    }

    pub fn keepalive(&self) -> Duration {
        Duration::from_millis(self.keepalive_ms.load(Ordering::Relaxed).max(10))
    }

    /// How often streams send a keep-alive (20 s; the tests make it shorter).
    pub fn set_keepalive(&self, d: Duration) {
        self.keepalive_ms.store(d.as_millis() as u64, Ordering::Relaxed);
    }

    /// Count a new event stream of `user`. A stream from the same device replaces the older one (a phone that
    /// changed networks may leave one behind that only times out much later). None: too many open already.
    pub fn open_stream(&self, user: &str, device: Option<&str>) -> Option<StreamTicket> {
        let mut all = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(d) = device {
            for s in all.iter().filter(|s| s.user == user && s.device.as_deref() == Some(d)) {
                s.close.notify_one();
            }
            all.retain(|s| !(s.user == user && s.device.as_deref() == Some(d)));
        }
        if all.len() >= MAX_ALL_STREAMS || all.iter().filter(|s| s.user == user).count() >= MAX_STREAMS {
            return None;
        }
        let id = self.stream_ids.fetch_add(1, Ordering::Relaxed);
        let close = Arc::new(tokio::sync::Notify::new());
        all.push(OpenStream { id, user: user.to_string(), device: device.map(String::from), close: close.clone() });
        Some(StreamTicket { live: self.streams.clone(), id, close })
    }

    /// Event streams open (of one user, or all).
    pub fn stream_count(&self, user: Option<&str>) -> usize {
        let all = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        all.iter().filter(|s| user.is_none_or(|u| s.user == u)).count()
    }

    /// Stop every event stream (the server is stopping: they'd keep it waiting).
    pub fn close_streams(&self) {
        for s in self.streams.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            s.close.notify_one();
        }
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
