//! Time limits shared by the streaming dictation workers (Deepgram, OpenAI
//! Realtime). Stop must come back even when the network does not: a
//! connect that never completes, a send into a socket nobody reads, or a
//! worker that is stuck anyway.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use futures_util::{Sink, SinkExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `stop()` waits for a cloud streaming worker before it gives up
/// on it and returns the text committed so far. Covers the tail flush plus
/// the drain window of a healthy session.
pub const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// The same for a local streaming engine, whose stop runs one last model
/// pass over the tail: seconds on a slow CPU, never minutes.
pub const LOCAL_STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(30);

pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Open the socket, giving up at `timeout` or as soon as `cancel` is set.
pub async fn connect<R: IntoClientRequest + Unpin>(
    request: R,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<Ws, String> {
    assert!(!timeout.is_zero(), "connect: zero timeout");
    tokio::select! {
        r = tokio::time::timeout(timeout, connect_async(request)) => match r {
            Ok(Ok((ws, _resp))) => Ok(ws),
            Ok(Err(e)) => Err(format!("connect failed: {e}")),
            Err(_) => Err(format!("connect timed out after {}s", timeout.as_secs())),
        },
        _ = cancelled(cancel) => Err("connect cancelled by stop".into()),
    }
}

/// One frame, within [`SEND_TIMEOUT`].
pub async fn send<S>(write: &mut S, msg: Message) -> Result<(), String>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    send_within(write, msg, SEND_TIMEOUT).await
}

async fn send_within<S>(write: &mut S, msg: Message, timeout: Duration) -> Result<(), String>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    match tokio::time::timeout(timeout, write.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("send timed out after {}ms", timeout.as_millis())),
    }
}

/// Join a streaming worker for at most `timeout`. A worker that does not
/// finish is left running detached.
pub fn join_worker(handle: JoinHandle<()>, timeout: Duration, tag: &str) {
    if let crate::meeting::BoundedJoin::TimedOut = crate::meeting::join_bounded(handle, timeout) {
        crate::log(&format!(
            "[{tag}] worker still busy after {}s; stop returns without it",
            timeout.as_secs()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    fn silent_listener() -> (TcpListener, String) {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("ws://{}/", l.local_addr().expect("addr"));
        (l, url)
    }

    #[test]
    fn a_server_that_never_answers_times_the_connect_out() {
        let (_l, url) = silent_listener();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let r = rt.block_on(connect(url, &cancel, Duration::from_millis(500)));
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn stop_cancels_a_connect_that_hangs() {
        let (_l, url) = silent_listener();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let r = rt.block_on(connect(url, &cancel, Duration::from_secs(60)));
        assert_eq!(r.err().as_deref(), Some("connect cancelled by stop"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_send_that_never_completes_times_out() {
        let mut stuck = Box::pin(futures_util::sink::unfold((), |_, _m: Message| {
            futures_util::future::pending::<Result<(), String>>()
        }));
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let started = Instant::now();
        let r = rt.block_on(send_within(
            &mut stuck,
            Message::Text("x".into()),
            Duration::from_millis(200),
        ));
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
