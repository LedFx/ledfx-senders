use super::config::{HueConfig, HueError};
use bytes::BytesMut;
use rtc_dtls::cipher_suite::CipherSuiteId;
use rtc_dtls::config::{ConfigBuilder, HandshakeConfig};
use rtc_dtls::endpoint::{Endpoint, EndpointEvent};
use rtc_shared::TransportProtocol;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

pub struct Session {
    endpoint: Endpoint,
    handshake: Arc<HandshakeConfig>,
    peer: SocketAddr,
    connected: bool,
}

impl Session {
    pub fn new(config: &HueConfig, local: SocketAddr) -> Result<Self, HueError> {
        let provider = Arc::new(rtc_crypto::providers::RingProvider::new());
        let key = config.key.clone();
        let handshake = ConfigBuilder::default()
            .with_crypto_provider(provider)
            .with_cipher_suites(vec![CipherSuiteId::Tls_Psk_With_Aes_128_Gcm_Sha256])
            .with_psk(Some(Arc::new(move |_| Ok(key.as_ref().to_vec()))))
            .with_psk_identity_hint(Some(config.identity.clone()))
            .build(true, Some(config.peer))
            .map_err(|_| HueError::Configuration("invalid DTLS configuration"))?;
        Ok(Self {
            endpoint: Endpoint::new(local, TransportProtocol::UDP, None),
            handshake: Arc::new(handshake),
            peer: config.peer,
            connected: false,
        })
    }

    pub fn start(&mut self, now: Instant) -> Result<(), HueError> {
        self.endpoint
            .connect(now, self.peer, self.handshake.clone(), None)
            .map_err(|_| HueError::Protocol("DTLS operation failed"))
    }

    pub fn read(&mut self, now: Instant, bytes: Vec<u8>) -> Result<(), HueError> {
        let events = self
            .endpoint
            .read(now, self.peer, None, BytesMut::from(bytes.as_slice()))
            .map_err(|_| HueError::Protocol("DTLS operation failed"))?;
        // Hue has no receive API: drop established application data after the
        // adapter's bounded drain. Only the authenticated engine event connects.
        for event in events {
            if matches!(event, EndpointEvent::HandshakeComplete) {
                self.connected = true;
            }
        }
        Ok(())
    }

    pub fn write(&mut self, now: Instant, bytes: &[u8]) -> Result<(), HueError> {
        if !self.connected {
            return Err(HueError::Closed);
        }
        self.endpoint
            .write(now, self.peer, bytes)
            .map_err(|_| HueError::Protocol("DTLS operation failed"))
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn next_timeout(&self) -> Option<Instant> {
        self.endpoint.poll_timeout(&self.peer)
    }

    pub fn timeout(&mut self, now: Instant) -> Result<(), HueError> {
        self.endpoint
            .handle_timeout(self.peer, now)
            .map_err(|_| HueError::Protocol("DTLS operation failed"))
    }

    pub fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.endpoint
            .poll_transmit()
            .map(|message| message.message.to_vec())
    }

    pub fn close(&mut self, now: Instant) -> Result<(), HueError> {
        self.connected = false;
        self.endpoint
            .close(now)
            .map_err(|_| HueError::Protocol("DTLS operation failed"))
    }
}
