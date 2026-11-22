//! The live remote screen, this computer's side (contracts/companion/frames.json remoteScreen; the server only pipes it, rova-backend
//! ws/remoteRelay.ts). The screen goes out as JPEG frames on one WebSocket; the phone's touches and keys come back on it, each signed with the
//! session key only the phone and this computer hold, with a rising sequence number, and are put into the system at once.
//!
//! Speed on any line, mobile data included: a frame is sent only when the picture changed, at most two are in flight (the phone acknowledges
//! each), and the size and quality follow how fast the acknowledgements come back: smaller and lighter when the line is slow, sharper when it
//! keeps up. The phone's input never waits behind a frame.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use wiqo_core::relay::RemoteSession;
use wiqo_ops::remote::{self, Button, Input};

/// (width, JPEG quality) from the lightest to the sharpest; the session starts in the middle and moves with the line.
const LEVELS: [(u32, u8); 6] = [
    (640, 40),
    (800, 48),
    (1024, 55),
    (1280, 62),
    (1600, 70),
    (1920, 76),
];
const START_LEVEL: usize = 3;
const MAX_IN_FLIGHT: u32 = 2;
/// The fastest frame rate (a frame every 33 ms); an unchanged screen sends nothing.
const MIN_FRAME_GAP: Duration = Duration::from_millis(33);
/// The longest a session may last; the phone opens a new one when the person comes back.
const MAX_SESSION: Duration = Duration::from_secs(4 * 60 * 60);

/// What the capture loop and the input loop share.
struct State {
    in_flight: AtomicU32,
    sent_seq: AtomicU32,
    level: Mutex<usize>,
    /// Round trip of the last acknowledgement, in milliseconds.
    rtt_ms: AtomicU64,
    sent_at: Mutex<std::collections::HashMap<u32, Instant>>,
    /// The phone is on the other end (the server says so with `peer`).
    viewer: AtomicBool,
    /// Ask for a frame even if the screen looks the same (the phone just joined or asked for another quality).
    force: AtomicBool,
    max_width: u32,
    last_slow: Mutex<Option<Instant>>,
    last_good: Mutex<Instant>,
}

impl State {
    fn level(&self) -> (u32, u8) {
        let (width, quality) = LEVELS[*self.level.lock().expect("level lock")];
        (width.min(self.max_width), quality)
    }

    /// The line kept up for a while: one step sharper. It fell behind: one step lighter at once.
    fn adapt(&self, behind: bool) {
        let mut level = self.level.lock().expect("level lock");
        if behind {
            let mut slow = self.last_slow.lock().expect("slow lock");
            if slow.is_none_or(|at| at.elapsed() > Duration::from_millis(600)) && *level > 0 {
                *level -= 1;
                *slow = Some(Instant::now());
            }
            *self.last_good.lock().expect("good lock") = Instant::now();
        } else {
            let mut good = self.last_good.lock().expect("good lock");
            if good.elapsed() > Duration::from_secs(2)
                && self.rtt_ms.load(Ordering::Relaxed) < 180
                && *level + 1 < LEVELS.len()
            {
                *level += 1;
                *good = Instant::now();
            }
        }
    }
}

/// Serves one session until the phone leaves, the line drops or the session runs out. `working` is told when it starts and ends (the window shows it).
pub async fn serve(session: RemoteSession, working: impl Fn(bool) + Send + Sync + 'static) {
    working(true);
    if let Err(error) = run(session).await {
        tracing::warn!("remote screen: {error}");
    }
    working(false);
}

async fn run(session: RemoteSession) -> anyhow::Result<()> {
    let mut request = session.socket_url.as_str().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", session.access_token))?,
    );
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    let (mut sink, mut stream) = socket.split();
    tracing::info!("remote screen: joined");

    let state = Arc::new(State {
        in_flight: AtomicU32::new(0),
        sent_seq: AtomicU32::new(0),
        level: Mutex::new(START_LEVEL),
        rtt_ms: AtomicU64::new(0),
        sent_at: Mutex::new(std::collections::HashMap::new()),
        viewer: AtomicBool::new(true),
        force: AtomicBool::new(true),
        max_width: session.max_width,
        last_slow: Mutex::new(None),
        last_good: Mutex::new(Instant::now()),
    });

    // One writer for the socket: frames and the few text lines both go through it.
    let (outgoing, mut queue) = mpsc::channel::<Message>(8);
    let writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let (width, height) = remote::display_size();
    let allowed = remote::input_allowed();
    let _ = outgoing
        .send(Message::text(json!({ "t": "info", "w": width, "h": height, "os": std::env::consts::OS, "input": allowed }).to_string()))
        .await;
    if allowed == Some(false) {
        let _ = outgoing.send(Message::text(json!({ "t": "err", "code": "ACCESSIBILITY_PERMISSION_REQUIRED",
            "message": "Bu Mac'te Wiqo'ya Erişilebilirlik izni ver (Sistem Ayarları › Gizlilik ve Güvenlik › Erişilebilirlik); görüntü gelir ama dokunuşlar gitmez." }).to_string())).await;
    }

    // Input is put into the system on its own thread, in order, so a slow frame never delays a click.
    let (inputs, mut pending) = mpsc::unbounded_channel::<Input>();
    let injector = std::thread::spawn(move || {
        while let Some(input) = pending.blocking_recv() {
            if let Err(error) = remote::inject(&input) {
                tracing::debug!("remote input: {error}");
            }
        }
    });

    let capture = tokio::spawn(capture_loop(state.clone(), outgoing.clone()));
    let started = Instant::now();
    let mut last_seq: u64 = 0;
    let key = session.session_key.clone();

    loop {
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        if started.elapsed() > MAX_SESSION {
            break;
        }
        let message = match next {
            Err(_) => continue,
            Ok(None) | Ok(Some(Err(_))) => break,
            Ok(Some(Ok(message))) => message,
        };
        let Message::Text(text) = message else {
            continue;
        };
        let text = text.as_str();
        // The server's own line about the other side.
        if let Ok(value) = serde_json::from_str::<Value>(text) {
            if value["t"] == "peer" {
                let on = value["on"].as_bool().unwrap_or(false);
                state.viewer.store(on, Ordering::Relaxed);
                if on {
                    state.force.store(true, Ordering::Relaxed);
                }
                continue;
            }
        }
        let Some(value) = verified(&key, text, &mut last_seq) else {
            tracing::warn!("remote screen: an input without a valid signature was dropped");
            continue;
        };
        match value["t"].as_str().unwrap_or("") {
            "bye" => break,
            "ack" => {
                if let Some(seq) = value["f"].as_u64() {
                    let sent = state
                        .sent_at
                        .lock()
                        .expect("sent lock")
                        .remove(&(seq as u32));
                    if let Some(at) = sent {
                        state
                            .rtt_ms
                            .store(at.elapsed().as_millis() as u64, Ordering::Relaxed);
                    }
                    release(&state.in_flight, 1);
                    state.adapt(false);
                }
            }
            "hello" | "refresh" => state.force.store(true, Ordering::Relaxed),
            "quality" => {
                let mut level = state.level.lock().expect("level lock");
                *level = match value["v"].as_str() {
                    Some("high") => LEVELS.len() - 1,
                    Some("low") => 0,
                    _ => START_LEVEL,
                };
                state.force.store(true, Ordering::Relaxed);
            }
            _ => {
                if let Some(input) = input_of(&value) {
                    let _ = inputs.send(input);
                    // What the touch changed shows at once, not on the next change the capture loop happens to see.
                    state.force.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    capture.abort();
    drop(inputs);
    drop(outgoing);
    let _ = writer.await;
    let _ = tokio::task::spawn_blocking(move || injector.join()).await;
    tracing::info!("remote screen: ended");
    Ok(())
}

/// Frames no longer in flight (acknowledged, or given up as lost); never below zero.
fn release(counter: &AtomicU32, count: u32) {
    let mut current = counter.load(Ordering::Relaxed);
    while let Err(actual) = counter.compare_exchange_weak(
        current,
        current.saturating_sub(count),
        Ordering::Relaxed,
        Ordering::Relaxed,
    ) {
        current = actual;
    }
}

/// The message, when its signature holds and its sequence number is newer than every one before (a replayed input is dropped).
fn verified(key: &str, text: &str, last_seq: &mut u64) -> Option<Value> {
    let (signature, body) = text.split_once('.')?;
    if !wiqo_contract::verify_signature(
        &wiqo_contract::remote_input_signature(key, body),
        signature,
    ) {
        return None;
    }
    let value: Value = serde_json::from_str(body).ok()?;
    let seq = value["s"].as_u64()?;
    if seq <= *last_seq {
        return None;
    }
    *last_seq = seq;
    Some(value)
}

fn fraction(value: &Value, name: &str) -> Option<f64> {
    value[name]
        .as_f64()
        .filter(|number| number.is_finite())
        .map(|number| number.clamp(0.0, 1.0))
}

fn button(value: &Value) -> Button {
    if value["b"] == "right" {
        Button::Right
    } else {
        Button::Left
    }
}

fn input_of(value: &Value) -> Option<Input> {
    let at = || Some((fraction(value, "x")?, fraction(value, "y")?));
    Some(match value["t"].as_str()? {
        "move" => {
            let (x, y) = at()?;
            Input::Move { x, y }
        }
        "down" => {
            let (x, y) = at()?;
            Input::Down {
                x,
                y,
                button: button(value),
            }
        }
        "up" => {
            let (x, y) = at()?;
            Input::Up {
                x,
                y,
                button: button(value),
            }
        }
        "click" => {
            let (x, y) = at()?;
            Input::Click {
                x,
                y,
                button: button(value),
                count: value["n"].as_u64().unwrap_or(1).clamp(1, 3) as u8,
            }
        }
        "scroll" => {
            let (x, y) = at()?;
            Input::Scroll {
                x,
                y,
                dx: value["dx"].as_f64().unwrap_or(0.0) as i32,
                dy: value["dy"].as_f64().unwrap_or(0.0) as i32,
            }
        }
        "text" => Input::Text(value["v"].as_str()?.chars().take(2_000).collect()),
        "key" => Input::Key {
            key: value["k"].as_str()?.to_ascii_lowercase(),
            modifiers: value["m"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        },
        _ => return None,
    })
}

/// Captures and sends while the phone is there and the line has room; skips a frame that shows nothing new.
async fn capture_loop(state: Arc<State>, outgoing: mpsc::Sender<Message>) {
    let mut last_digest: Option<u64> = None;
    let mut last_sent = Instant::now() - MIN_FRAME_GAP;
    loop {
        let wait = MIN_FRAME_GAP.saturating_sub(last_sent.elapsed());
        tokio::time::sleep(wait.max(Duration::from_millis(4))).await;
        if !state.viewer.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        if state.in_flight.load(Ordering::Relaxed) >= MAX_IN_FLIGHT {
            // The phone has not caught up: lighter frames, and an old unacknowledged one counts as lost after two seconds.
            state.adapt(true);
            let stale = {
                let mut sent = state.sent_at.lock().expect("sent lock");
                let before = sent.len();
                sent.retain(|_, at| at.elapsed() < Duration::from_secs(2));
                before - sent.len()
            };
            if stale > 0 {
                release(&state.in_flight, stale as u32);
            }
            continue;
        }
        let forced = state.force.swap(false, Ordering::Relaxed);
        let (width, quality) = state.level();
        let previous = if forced { None } else { last_digest };
        let frame = match tokio::task::spawn_blocking(move || {
            remote::capture(width, quality, previous)
        })
        .await
        {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) => continue,
            Ok(Err(error)) => {
                let _ = outgoing.send(Message::text(json!({ "t": "err", "code": "SCREEN_CAPTURE_FAILED",
                    "message": format!("Ekran alınamadı ({error}). Ekran Kaydı iznini kontrol et.") }).to_string())).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(_) => continue,
        };
        last_digest = Some(frame.digest);
        let seq = state.sent_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let mut packet = Vec::with_capacity(frame.jpeg.len() + 12);
        packet.extend_from_slice(&[b'W', 1, u8::from(forced), 0]);
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&(frame.width.min(u32::from(u16::MAX)) as u16).to_be_bytes());
        packet.extend_from_slice(&(frame.height.min(u32::from(u16::MAX)) as u16).to_be_bytes());
        packet.extend_from_slice(&frame.jpeg);
        state
            .sent_at
            .lock()
            .expect("sent lock")
            .insert(seq, Instant::now());
        state.in_flight.fetch_add(1, Ordering::Relaxed);
        last_sent = Instant::now();
        if outgoing.send(Message::binary(packet)).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(key: &str, body: &str) -> String {
        format!(
            "{}.{body}",
            wiqo_contract::remote_input_signature(key, body)
        )
    }

    #[test]
    fn only_signed_inputs_with_a_rising_sequence_are_taken() {
        let key = wiqo_contract::remote_session_key(
            "pairing-token",
            "s".repeat(32).as_str(),
            "n".repeat(16).as_str(),
        );
        let mut last = 0;
        let first = signed(&key, r#"{"s":1,"t":"click","x":0.5,"y":0.5}"#);
        assert!(verified(&key, &first, &mut last).is_some());
        assert!(
            verified(&key, &first, &mut last).is_none(),
            "a replay is dropped"
        );
        assert!(
            verified(
                "other-key",
                &signed("other-key", r#"{"s":2,"t":"text","v":"a"}"#),
                &mut last
            )
            .is_some(),
            "own key works"
        );
        let forged = format!("{}.{}", "0".repeat(64), r#"{"s":9,"t":"text","v":"x"}"#);
        assert!(
            verified(&key, &forged, &mut last).is_none(),
            "a wrong signature is dropped"
        );
    }

    #[test]
    fn inputs_are_read_from_their_messages() {
        let click = input_of(&json!({ "t": "click", "x": 0.25, "y": 2.0, "b": "right", "n": 2 }));
        assert!(
            matches!(click, Some(Input::Click { x, y, button: Button::Right, count: 2 }) if (x - 0.25).abs() < 1e-9 && (y - 1.0).abs() < 1e-9)
        );
        assert!(
            matches!(input_of(&json!({ "t": "key", "k": "Backspace" })), Some(Input::Key { ref key, .. }) if key == "backspace")
        );
        assert!(input_of(&json!({ "t": "nonsense" })).is_none());
    }
}
