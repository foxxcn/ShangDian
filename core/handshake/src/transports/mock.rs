use async_channel::bounded;
use async_trait::async_trait;
use axum::routing::get;
use axum::Router;
use bytes::{BufMut, Bytes, BytesMut};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use super::{Transport, TransportReceiver, TransportSender};
use crate::schema;
use crate::shutdown::ShutdownWaiter;

static LISTENERS: OnceCell<
    DashMap<u16, tokio::sync::mpsc::Sender<(MockTransportSender, MockTransportReceiver)>>,
> = OnceCell::const_new();

/// Dial a mocked connection, returning a tokio sender and receiver for the "client".
/// Users are in charge of sending the initial [`HandshakeRequestFrame`].
pub async fn dial_mock(
    port: u16,
) -> Option<(async_channel::Sender<Bytes>, async_channel::Receiver<Bytes>)> {
    let map = LISTENERS.get_or_init(|| async { DashMap::new() }).await;
    let conn_tx = map.get(&port)?;

    let (tx1, rx2) = bounded(256);
    let (tx2, rx1) = bounded(256);
    conn_tx
        .send((
            MockTransportSender {
                tx: tx1,
                current_write: 0,
                buffer: BytesMut::new(),
            },
            MockTransportReceiver { rx: rx1 },
        ))
        .await
        .ok()?;

    Some((tx2, rx2))
}

/// Mock memory transport backed by tokio channels
pub struct MockTransport {
    port: u16,
    conn_rx: tokio::sync::mpsc::Receiver<(MockTransportSender, MockTransportReceiver)>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct MockTransportConfig {
    port: u16,
}

impl Drop for MockTransport {
    fn drop(&mut self) {
        let map = LISTENERS.get().unwrap();
        map.remove(&self.port);
    }
}

#[async_trait]
impl Transport for MockTransport {
    type Config = MockTransportConfig;
    type Sender = MockTransportSender;
    type Receiver = MockTransportReceiver;

    async fn bind(
        _waiter: ShutdownWaiter,
        config: Self::Config,
    ) -> anyhow::Result<(Self, Option<Router>)> {
        let (conn_tx, conn_rx) = tokio::sync::mpsc::channel(8);

        let map = LISTENERS.get_or_init(|| async { DashMap::new() }).await;
        assert!(!map.contains_key(&config.port));
        map.insert(config.port, conn_tx);

        Ok((
            Self {
                port: config.port,
                conn_rx,
            },
            Some(Router::new().route("/mock", get(|| async { "mock is enabled" }))),
        ))
    }

    /// accept a new connection. This will immediately await the handshake frame after the
    /// connection is established.
    async fn accept(
        &mut self,
    ) -> Option<(schema::HandshakeRequestFrame, Self::Sender, Self::Receiver)> {
        let (sender, receiver) = self.conn_rx.recv().await?;

        // decode handshake frame
        let bytes = receiver.rx.recv().await.ok()?;
        let frame = schema::HandshakeRequestFrame::decode(&bytes).ok()?;

        Some((frame, sender, receiver))
    }
}

/// Mock sender
pub struct MockTransportSender {
    tx: async_channel::Sender<Bytes>,
    current_write: usize,
    buffer: BytesMut,
}

impl MockTransportSender {
    fn send_inner(&mut self, bytes: Bytes) {
        self.tx
            .try_send(bytes)
            .expect("failed to send bytes over the mock connection")
    }
}

impl TransportSender for MockTransportSender {
    fn send_handshake_response(&mut self, response: schema::HandshakeResponse) {
        self.send_inner(response.encode());
    }

    fn send(&mut self, frame: schema::ResponseFrame) {
        self.send_inner(frame.encode());
    }

    fn start_write(&mut self, len: usize) {
        debug_assert!(self.buffer.is_empty());
        self.buffer.reserve(len);
    }

    fn write(&mut self, buf: &[u8]) -> anyhow::Result<usize> {
        debug_assert!(buf.len() <= self.current_write);
        self.current_write -= buf.len();
        self.buffer.put(buf);
        if self.current_write == 0 {
            let bytes = self.buffer.split().freeze();
            self.send_inner(bytes);
        }
        Ok(buf.len())
    }
}

/// Mock receiver
pub struct MockTransportReceiver {
    rx: async_channel::Receiver<Bytes>,
}

#[async_trait]
impl TransportReceiver for MockTransportReceiver {
    async fn recv(&mut self) -> Option<schema::RequestFrame> {
        let bytes = self.rx.recv().await.ok()?;
        Some(schema::RequestFrame::decode(&bytes).expect("failed to decode request frame"))
    }
}

#[cfg(test)]
mod tests {
    use fleek_crypto::{ClientPublicKey, ClientSignature};

    use super::*;
    use crate::shutdown::ShutdownNotifier;

    #[tokio::test]
    async fn dial() -> anyhow::Result<()> {
        let notifier = ShutdownNotifier::default();
        let mut server =
            MockTransport::bind(notifier.waiter(), MockTransportConfig { port: 420 }).await?;

        let client = dial_mock(420).await.unwrap();
        // send the initial handshake
        client
            .0
            .send(
                schema::HandshakeRequestFrame::Handshake {
                    retry: None,
                    service: 0,
                    pk: ClientPublicKey([1; 96]),
                    pop: ClientSignature([2; 48]),
                }
                .encode(),
            )
            .await?;

        assert!(server.0.accept().await.is_some());

        Ok(())
    }
}
