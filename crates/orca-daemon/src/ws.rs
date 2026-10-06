//! WebSocket liveness: server-side pings and dead-peer detection.
//!
//! Five endpoints here hold a WebSocket open (`container_terminal_ws`,
//! `k8s_pod_terminal_ws`, `k8s_enable_ws`, `tunnel_ws`). Before this module they
//! had **no ping, no pong handling and no read timeout**, so a client that
//! vanished without a TCP FIN — a laptop that slept, a VPN that dropped, a
//! webview killed by the OS — left the server side parked in `ws_receiver.next()`
//! indefinitely. The task, the `docker exec` stream or the `kubectl` child stayed
//! alive behind it.
//!
//! # Why not simply time out an idle read
//!
//! Because for a terminal, "no frames for a while" is the *normal* state: a user
//! leaving a shell open and not typing is not an error. A naive read timeout
//! therefore kills healthy sessions, which is a net regression.
//!
//! The correct liveness signal is a **ping/pong round trip**, and that is what
//! this module implements: the server sends a `Ping` every [`PING_INTERVAL`], and
//! per RFC 6455 the peer must answer with a `Pong`. Any inbound frame counts as
//! proof of life, so a `Pong` keeps an otherwise silent terminal alive — while a
//! peer that is *gone* stops answering, and is dropped after [`IDLE_TIMEOUT`].
//!
//! Both durations are parameters rather than hard-coded so the behaviour can be
//! tested against a real socket without waiting 45 seconds.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How often an otherwise-silent server sends a `Ping`.
pub const PING_INTERVAL: Duration = Duration::from_secs(15);

/// How long a connection may go without *any* inbound frame before it is
/// considered dead. Deliberately a multiple of [`PING_INTERVAL`]: one dropped
/// ping (scheduling jitter, a paused VM) must not be mistaken for a dead peer.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// Outcome of reading one frame with a liveness deadline.
#[derive(Debug)]
pub enum RecvOutcome {
    /// A frame arrived. Anything counts as proof of life, including a `Pong`.
    Message(Message),
    /// The peer closed the connection cleanly (or the stream ended).
    Closed,
    /// Nothing arrived within the deadline: the peer is presumed gone.
    Idle,
}

/// Read one frame, giving up after `idle`.
///
/// Wrap every `ws_receiver.next()` in this. A read timeout on a terminal is only
/// safe *because* the server pings: the peer has a contractual obligation to
/// answer, so silence means the peer is gone rather than merely quiet.
pub async fn recv_with_idle(recv: &mut SplitStream<WebSocket>, idle: Duration) -> RecvOutcome {
    match tokio::time::timeout(idle, recv.next()).await {
        Ok(Some(Ok(msg))) => RecvOutcome::Message(msg),
        // `Err` is a protocol/transport error, and `None` means the stream
        // ended. Both are terminal: there is no connection left to keep alive.
        Ok(Some(Err(_)) | None) => RecvOutcome::Closed,
        Err(_elapsed) => RecvOutcome::Idle,
    }
}

/// Owns the write half of a WebSocket and sends a `Ping` when it goes quiet.
///
/// Handlers send through the returned channel instead of touching the sink, so
/// that application frames and keepalive pings cannot interleave mid-frame. When
/// the last sender is dropped the task exits and closes the socket, which is how
/// a handler signals "I'm done".
///
/// A failed send means the socket is gone; the task then exits, and the read side
/// notices via its own deadline.
pub fn spawn_pinger(
    sink: SplitSink<WebSocket, Message>,
    interval: Duration,
) -> (mpsc::Sender<Message>, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Message>(64);

    let handle = tokio::spawn(async move {
        let mut sink = sink;
        loop {
            tokio::select! {
                queued = rx.recv() => match queued {
                    Some(msg) => {
                        if sink.send(msg).await.is_err() {
                            break;
                        }
                    }
                    // Every sender dropped: the handler finished its work.
                    None => {
                        let _ = sink.close().await;
                        break;
                    }
                },
                _ = tokio::time::sleep(interval) => {
                    // `Ping` payload is echoed back in the `Pong`, which makes the
                    // reply self-identifying in a packet capture.
                    if sink.send(Message::Ping(b"orca".to_vec().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    (tx, handle)
}

/// Stop a pinger, giving it a bounded chance to flush what is already queued.
///
/// Dropping the last sender is what tells the pinger to finish, and `recv()`
/// returns `None` only once the channel is *empty* — so a `Close` queued just
/// before teardown is still delivered. That is why this does not simply call
/// `handle.abort()`: aborting can discard a `Close` that the handler deliberately
/// queued to tell the client the stream ended (the tunnel does exactly that), which
/// is a worse outcome than the old code's awaited `sink.close()`.
///
/// The timeout matters because the peer may be gone and the socket may never
/// drain; after `grace` the task is detached and the socket drops with it. Callers
/// must drop *every* sender first (aborting the tasks that hold clones), otherwise
/// `recv()` never sees the channel close and this always waits the full grace.
pub async fn shutdown_pinger(tx: mpsc::Sender<Message>, handle: JoinHandle<()>, grace: Duration) {
    drop(tx);
    let _ = tokio::time::timeout(grace, handle).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_timeout_tolerates_one_missed_ping() {
        // If IDLE_TIMEOUT were <= PING_INTERVAL a single scheduling hiccup would
        // look like a dead peer and drop healthy terminals.
        assert!(
            IDLE_TIMEOUT >= PING_INTERVAL * 2,
            "idle timeout {IDLE_TIMEOUT:?} must tolerate at least one missed ping \
             (interval {PING_INTERVAL:?})",
        );
    }
}

/// End-to-end tests over a real socket.
///
/// These exist because the previous round deliberately deferred this fix: it
/// compiles, and no unit test can tell whether it *works*. What has to be true is a
/// property of two connected peers — an idle-but-alive client must survive, and a
/// client that holds the TCP connection open but stops answering must eventually be
/// dropped. So these drive a real axum WebSocket server with a real client, using
/// short intervals (hundreds of milliseconds) so they stay fast.
#[cfg(test)]
mod real_socket_tests {
    use super::*;
    use axum::Router;
    use axum::extract::ws::WebSocketUpgrade;
    use axum::routing::get;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::tungstenite;

    type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    /// A server shaped like `handle_terminal`: a pinger plus an idle-deadline read
    /// loop. It reports on `done` when its read loop exits, which is how the tests
    /// observe "the server gave up on this peer".
    async fn idle_sensitive_server(socket: WebSocket, ping: Duration, idle: Duration, done: oneshot::Sender<()>) {
        let (sender, mut receiver) = socket.split();
        let (tx, pinger) = spawn_pinger(sender, ping);
        loop {
            match recv_with_idle(&mut receiver, idle).await {
                RecvOutcome::Message(Message::Text(text)) => {
                    // Echo, so a test can also confirm data still flows.
                    if tx.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                // A `Pong` (or anything else) is proof of life.
                RecvOutcome::Message(_) => continue,
                RecvOutcome::Closed | RecvOutcome::Idle => break,
            }
        }
        drop(tx);
        pinger.abort();
        let _ = done.send(());
    }

    /// Start the server above on an ephemeral port.
    async fn serve(ping: Duration, idle: Duration) -> (String, oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
        let (done_tx, done_rx) = oneshot::channel();
        // Passed through a `Router` extension rather than captured by the handler
        // closure: a closure taking `WebSocketUpgrade` does not satisfy axum's
        // `Handler` bounds, and a plain `fn` keeps the types obvious.
        let app = Router::new()
            .route("/ws", get(ws_entry))
            .layer(axum::Extension(TestParams {
                ping,
                idle,
                done: std::sync::Arc::new(tokio::sync::Mutex::new(Some(done_tx))),
            }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("ws://{addr}/ws"), done_rx, handle)
    }

    /// `oneshot::Sender` is not `Clone`, but `Extension` requires `Clone` — hence
    /// the `Arc<Mutex<Option<..>>>`, which also lets the handler take it exactly
    /// once.
    #[derive(Clone)]
    struct TestParams {
        ping: Duration,
        idle: Duration,
        done: std::sync::Arc<tokio::sync::Mutex<Option<oneshot::Sender<()>>>>,
    }

    async fn ws_entry(
        ws: WebSocketUpgrade,
        axum::Extension(params): axum::Extension<TestParams>,
    ) -> axum::response::Response {
        let done = params.done.lock().await.take();
        ws.on_upgrade(move |socket| async move {
            if let Some(done) = done {
                idle_sensitive_server(socket, params.ping, params.idle, done).await
            }
        })
    }

    async fn connect(url: &str) -> Client {
        let (client, _resp) = tokio_tungstenite::connect_async(url)
            .await
            .expect("websocket handshake");
        client
    }

    /// The server must probe an otherwise-silent client, so that silence can be
    /// read as "gone" rather than "not typing".
    #[tokio::test]
    async fn the_server_pings_a_silent_client() {
        let (url, _done, server) = serve(Duration::from_millis(150), Duration::from_secs(5)).await;
        let mut client = connect(&url).await;

        // Send nothing at all. The server should ping on its own.
        let msg = tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .expect("a ping should arrive without the client sending anything")
            .expect("stream open")
            .expect("frame decodes");

        match msg {
            tungstenite::Message::Ping(payload) => {
                assert_eq!(payload.as_ref(), b"orca", "unexpected ping payload");
            }
            // Accepting anything else here would hide a regression, so only Ping passes.
            other => panic!("expected a Ping, got {other:?}"),
        }
        server.abort();
    }

    /// The regression that would make a naive fix wrong: an idle *but alive*
    /// terminal must not be killed. The client sends no application data, yet
    /// answers pings (tokio-tungstenite does that automatically when polled), so
    /// the server must keep it.
    #[tokio::test]
    async fn an_idle_but_answering_client_is_not_disconnected() {
        let idle = Duration::from_millis(600);
        let (url, mut done, server) = serve(Duration::from_millis(150), idle).await;
        let mut client = connect(&url).await;

        // Stay alive well past IDLE_TIMEOUT, sending no application data. Reading
        // is what makes tungstenite answer the server's pings.
        let deadline = tokio::time::Instant::now() + idle * 3;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(100), client.next()).await {
                Ok(Some(Ok(_))) => {}
                Ok(other) => panic!("connection dropped while idle: {other:?}"),
                Err(_) => {} // no frame this tick — fine
            }
        }

        assert!(
            done.try_recv().is_err(),
            "the server dropped an idle client that was still answering pings",
        );
        server.abort();
    }

    /// The actual bug: a peer that keeps the TCP connection open but never answers
    /// must be given up on. A laptop that slept or a webview killed by the OS looks
    /// exactly like this — no FIN, just silence. Before the fix the handler parked
    /// on `next()` forever, holding the exec stream / kubectl child behind it.
    #[tokio::test]
    async fn a_peer_that_stops_answering_is_eventually_dropped() {
        let idle = Duration::from_millis(600);
        let (url, mut done, server) = serve(Duration::from_millis(150), idle).await;

        // Handshake, then deliberately never poll the socket: the client neither
        // sends nor reads, so it never answers the server's pings while the TCP
        // connection stays open.
        let _client = connect(&url).await;

        tokio::time::timeout(idle * 5, &mut done)
            .await
            .expect("server never gave up on a peer that stopped answering")
            .expect("server should report through the channel");
        server.abort();
    }

    /// A server that immediately queues a `Close` and then performs the graceful
    /// pinger shutdown, modelling what `handle_tunnel` does when the TCP side ends.
    async fn close_immediately_server(socket: WebSocket) {
        let (sender, _receiver) = socket.split();
        let (tx, pinger) = spawn_pinger(sender, Duration::from_secs(30));
        let _ = tx.send(Message::Close(None)).await;
        shutdown_pinger(tx, pinger, Duration::from_secs(2)).await;
    }

    async fn close_entry(ws: WebSocketUpgrade) -> axum::response::Response {
        ws.on_upgrade(close_immediately_server)
    }

    /// `shutdown_pinger` must actually flush a `Close` the handler queued, not
    /// discard it. This is the regression risk introduced by moving writes behind
    /// the pinger: the tunnel queues `Close` and then tears down, so an
    /// implementation that just calls `handle.abort()` would silently drop that
    /// frame and leave the client to discover the end of the stream by timing out.
    ///
    /// Driven through a real socket on purpose — asserting on the channel would
    /// only re-test `mpsc`, which is precisely the mistake this guards against.
    #[tokio::test]
    async fn shutdown_pinger_flushes_a_queued_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, Router::new().route("/ws", get(close_entry))).await;
        });

        let mut client = connect(&format!("ws://{addr}/ws")).await;

        // The client must observe the Close rather than a bare TCP teardown.
        let frame = tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .expect("no frame arrived; the queued Close was discarded")
            .expect("stream open")
            .expect("frame decodes");

        assert!(
            matches!(frame, tungstenite::Message::Close(_)),
            "expected a Close frame, got {frame:?}",
        );
        server.abort();
    }
}
