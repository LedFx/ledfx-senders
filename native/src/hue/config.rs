use rtc_crypto::SecretVec;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HueError {
    Configuration,
    Dtls,
    Io,
    Timeout,
    Closed,
}

impl fmt::Display for HueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "configuration",
            Self::Dtls => "dtls",
            Self::Io => "io",
            Self::Timeout => "timeout",
            Self::Closed => "closed",
        })
    }
}

impl std::error::Error for HueError {}

pub struct HueConfig {
    pub peer: SocketAddr,
    pub identity: Vec<u8>,
    pub key: SecretVec,
    pub connect: Duration,
    pub send: Duration,
    pub close: Duration,
}

impl HueConfig {
    pub fn new(
        peer: SocketAddr,
        identity: Vec<u8>,
        key: Vec<u8>,
        connect: Duration,
        send: Duration,
        close: Duration,
    ) -> Result<Self, HueError> {
        if peer.port() == 0
            || identity.is_empty()
            || identity.len() > u16::MAX as usize
            || key.is_empty()
            || key.len() > u16::MAX as usize
            || connect.is_zero()
            || send.is_zero()
            || close.is_zero()
        {
            return Err(HueError::Configuration);
        }
        Ok(Self {
            peer,
            identity,
            key: SecretVec::new(key),
            connect,
            send,
            close,
        })
    }
}

pub struct Cancellation {
    cancelled: AtomicBool,
    mutex: Mutex<()>,
    wake: Condvar,
}

impl Cancellation {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            mutex: Mutex::new(()),
            wake: Condvar::new(),
        }
    }

    pub fn cancel(&self) {
        // Synchronize notification with wait's predicate to prevent lost wakeups.
        let _guard = self.mutex.lock().unwrap_or_else(|error| error.into_inner());
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn wait(&self, until: Instant) -> Result<(), HueError> {
        let guard = self.mutex.lock().map_err(|_| HueError::Closed)?;
        let (_guard, _) = self
            .wake
            .wait_timeout_while(
                guard,
                until.saturating_duration_since(Instant::now()),
                |_| !self.cancelled(),
            )
            .map_err(|_| HueError::Closed)?;
        if self.cancelled() {
            Err(HueError::Closed)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_require_protocol_representable_lengths() {
        let peer = "127.0.0.1:2100".parse().unwrap();
        let budget = Duration::from_secs(1);
        for length in [0, u16::MAX as usize, u16::MAX as usize + 1] {
            let identity =
                HueConfig::new(peer, vec![1; length], vec![2; 16], budget, budget, budget);
            let key = HueConfig::new(peer, vec![1; 8], vec![2; length], budget, budget, budget);
            let representable = length == u16::MAX as usize;
            assert_eq!(identity.is_ok(), representable, "identity length {length}");
            assert_eq!(key.is_ok(), representable, "key length {length}");
        }
    }
}
