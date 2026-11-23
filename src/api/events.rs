//! `encore.events`: one stream of server-sent events (`text/event-stream`) per app, telling it at once what
//! changed for its account, so it needn't long-poll or ask on a timer: its devices and the commands sent to it,
//! its Jam, its synced settings, its playlists, and what everyone sees (now playing, the library scan).
//!
//! Events are hints, small and without the thing itself: the app fetches what changed with the calls it already
//! has (devicePoll, jamPoll, syncGet, getPlaylists…). The stream reads the event log in `crate::live`, woken by
//! the same change counter as the long-polls.
//!
//! - Each event has an `id:`; an app that reconnects with `Last-Event-ID` (or `lastEventId=`) gets what it missed,
//!   or a `resync` event when that's no longer kept (or came from before the server restarted): then it fetches
//!   everything it shows anew.
//! - A keep-alive comment every 20 s (proxies close quiet connections) also carries the newest id.
//! - Nothing piles up for an app that doesn't read: past `QUEUE` chunks waiting, or what was sent before one
//!   keep-alive still not taken at the next, the stream is closed (the app reconnects and resumes).
//! - At most `live::MAX_STREAMS` per account; a stream from the same device (`id`) replaces its older one.
//! - The login is checked again at every keep-alive: a changed password or a deleted API key ends the stream.

use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    http::{StatusCode, header},
};
use serde_json::{Value, json};
use tokio::{sync::mpsc, time::Instant};

use super::{ApiError, Ctx, Reply, Shared, raw};
use crate::live::{self, Audience, Event, Since};

/// Chunks (one per wake) waiting for an app to take them; one that falls this far behind is dropped.
const QUEUE: usize = 64;
/// More events than this at once (a big import, say): one `resync` instead.
const MAX_BATCH: usize = 32;
/// How long an app waits before reconnecting (the SSE `retry:` field), ms.
const RETRY_MS: u64 = 5000;
/// What the stream may send.
pub const KINDS: &[&str] = &["hello", "resync", "devices", "command", "jam", "sync", "nowPlaying", "scan", "playlist"];

fn device_ok(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// How the stream's app signed in, to check again now and then.
enum Login {
    Key(String),
    Password(String),
}

impl Login {
    async fn still_good(&self, st: &Shared, user: &str) -> bool {
        // (a busy database isn't a reason to drop anyone: only a clear "no")
        match self {
            Login::Key(hash) => sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM api_key WHERE hash = ? AND user_id = ?")
                .bind(hash)
                .bind(user)
                .fetch_one(&st.db)
                .await
                .map_or(true, |n| n > 0),
            Login::Password(enc) => sqlx::query_scalar::<_, String>("SELECT password_enc FROM user WHERE id = ?")
                .bind(user)
                .fetch_optional(&st.db)
                .await
                .map_or(true, |p| p.as_deref() == Some(enc.as_str())),
        }
    }
}

fn frame(out: &mut String, id: Option<&str>, kind: &str, data: &Value) {
    if let Some(id) = id {
        out.push_str("id: ");
        out.push_str(id);
        out.push('\n');
    }
    out.push_str("event: ");
    out.push_str(kind);
    out.push_str("\ndata: ");
    out.push_str(&data.to_string()); // (one line: JSON as serde writes it has no newlines)
    out.push_str("\n\n");
}

/// The same event twice in one go is sent once (at its last place).
fn coalesce(events: Vec<Event>) -> Vec<Event> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Event> = events.into_iter().rev().filter(|e| seen.insert((e.kind, e.data.to_string()))).collect();
    out.reverse();
    out
}

/// The device a stream says it is stays listed as online while the stream is open (as devicePoll keeps it).
fn touch(st: &Shared, user: &str, device: Option<&str>) {
    if let Some(d) = device
        && let Some(x) = st.live.lock().devices.get_mut(&(user.to_string(), d.to_string()))
    {
        x.seen_ms = live::now_ms();
    }
}

pub async fn stream(c: &Ctx) -> Result<Reply, ApiError> {
    let device = c.p.get("id").filter(|d| device_ok(d)).map(String::from);
    let resume = c
        .headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .or(c.p.get("lastEventId"))
        .filter(|s| !s.trim().is_empty())
        .map(String::from);
    let live = &c.st.live;
    let Some(ticket) = live.open_stream(&c.user.id, device.as_deref()) else {
        return Err(ApiError { http: StatusCode::TOO_MANY_REQUESTS, ..ApiError::new(0, "Too many event streams open for this account") });
    };
    watch_scans(&c.st);
    let last = live.last_event();
    // where to start: after what the app saw, when that's still kept; else from now, and the app fetches anew
    let (start, resumed, resync) = match resume.as_deref().map(|r| live.parse_event_id(r)) {
        None => (last, false, false),
        Some(Some(n)) if matches!(live.events_since(n, &c.user.id, device.as_deref()), Since::New { .. }) => (n, true, false),
        Some(_) => (last, false, true),
    };
    touch(&c.st, &c.user.id, device.as_deref());
    let mut first = format!("retry: {RETRY_MS}\n");
    let id = live.event_id(start);
    let hello = json!({ "keepalive": live.keepalive().as_secs_f64(), "resumed": resumed, "events": KINDS });
    frame(&mut first, Some(&id), "hello", &hello);
    if resync {
        frame(&mut first, Some(&id), "resync", &json!({}));
    }
    let (tx, rx) = mpsc::channel::<Bytes>(QUEUE);
    let mut conn = Out { tx, sent: 0, taken: Arc::default() };
    conn.send(first); // at once: the app (and any proxy) knows the stream is open
    // (the login itself, not one of this call's parameters)
    let login = match c.p.0.iter().find(|(k, _)| k == "apiKey") {
        Some((_, k)) => Login::Key(crate::auth::sha256(k)),
        None => Login::Password(c.user.password_enc.clone()),
    };
    let taken = conn.taken.clone();
    tokio::spawn(run(c.st.clone(), c.user.id.clone(), device, login, start, conn, ticket));
    let body = Body::from_stream(futures_util::stream::unfold(rx, move |mut rx| {
        let taken = taken.clone();
        async move {
            let b = rx.recv().await?;
            taken.fetch_add(1, Ordering::Relaxed);
            Some((Ok::<Bytes, Infallible>(b), rx))
        }
    }));
    Ok(raw(
        StatusCode::OK,
        &[
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8".into()),
            (header::CACHE_CONTROL, "no-cache, no-store, no-transform".into()),
            (header::HeaderName::from_static("x-accel-buffering"), "no".into()), // nginx: don't hold it back
        ],
        body,
    ))
}

/// What goes to the app: chunks sent, and how many of them the connection has taken so far.
struct Out {
    tx: mpsc::Sender<Bytes>,
    sent: u64,
    taken: Arc<AtomicU64>,
}

impl Out {
    /// Sent, or the stream is to end (the app is gone, or too far behind).
    fn send(&mut self, s: String) -> bool {
        let ok = self.tx.try_send(Bytes::from(s)).is_ok();
        self.sent += 1;
        ok
    }
}

async fn run(
    st: Shared,
    user: String,
    device: Option<String>,
    login: Login,
    mut after: u64,
    mut conn: Out,
    ticket: live::StreamTicket,
) {
    let mut tick = st.live.subscribe();
    let mut next_alive = Instant::now() + st.live.keepalive();
    let mut sent_by_last_alive = conn.sent; // what was sent up to the last keep-alive
    loop {
        let mut out = String::new();
        match st.live.events_since(after, &user, device.as_deref()) {
            Since::New { events, last } => {
                let events = coalesce(events);
                if events.len() > MAX_BATCH {
                    frame(&mut out, Some(&st.live.event_id(last)), "resync", &json!({}));
                } else {
                    for e in &events {
                        frame(&mut out, Some(&st.live.event_id(e.id)), e.kind, &e.data);
                    }
                }
                after = last;
            }
            Since::TooOld { last } => {
                frame(&mut out, Some(&st.live.event_id(last)), "resync", &json!({}));
                after = last;
            }
        }
        if !out.is_empty() && !conn.send(out) {
            return;
        }
        tokio::select! {
            r = tick.changed() => if r.is_err() { return },
            _ = tokio::time::sleep_until(next_alive) => {
                if conn.taken.load(Ordering::Relaxed) < sent_by_last_alive {
                    return; // what was sent a keep-alive ago still hasn't been taken: too slow, or gone
                }
                if !login.still_good(&st, &user).await {
                    return;
                }
                touch(&st, &user, device.as_deref());
                // (an id alone moves the app's Last-Event-ID on past events that weren't for it)
                if !conn.send(format!(": keep-alive\nid: {}\n\n", st.live.event_id(after))) {
                    return;
                }
                sent_by_last_alive = conn.sent;
                next_alive = Instant::now() + st.live.keepalive();
            }
            _ = ticket.closed() => return,
            _ = conn.tx.closed() => return,
        }
    }
}

/// Scan events from the scan's own wake-ups (`ScanStatus::changed`): started, progress at most once a second,
/// finished. One watcher per server, from the first stream on.
fn watch_scans(st: &Shared) {
    if st.live.scan_watch.swap(true, Ordering::SeqCst) {
        return;
    }
    let weak = Arc::downgrade(st);
    let scan = st.scan.clone();
    tokio::spawn(async move {
        let mut was = scan.scanning.load(Ordering::SeqCst);
        let mut last_scan = scan.last_scan.load(Ordering::SeqCst);
        let (mut count_sent, mut sent_at) = (scan.count.load(Ordering::SeqCst), Instant::now());
        loop {
            let woken = scan.changed.notified();
            tokio::pin!(woken);
            woken.as_mut().enable(); // before looking: a wake-up meanwhile isn't lost
            let scanning = scan.scanning.load(Ordering::SeqCst);
            let count = scan.count.load(Ordering::SeqCst);
            let ended = scan.last_scan.load(Ordering::SeqCst);
            let state = if scanning && !was {
                Some("started")
            } else if !scanning && (was || ended != last_scan) {
                Some("finished")
            } else if scanning && count != count_sent && sent_at.elapsed() >= Duration::from_secs(1) {
                Some("progress")
            } else {
                None
            };
            if let Some(state) = state {
                let Some(st) = weak.upgrade() else { return };
                st.live.publish("scan", Audience::Everyone, json!({ "state": state, "scanning": scanning, "count": count }));
                (count_sent, sent_at) = (count, Instant::now());
            }
            (was, last_scan) = (scanning, ended);
            if scanning && count != count_sent {
                let _ = tokio::time::timeout(Duration::from_secs(1), woken).await; // progress held back: soon
            } else {
                woken.await;
            }
        }
    });
}
