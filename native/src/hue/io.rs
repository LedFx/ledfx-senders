use super::config::{Cancellation, HueConfig, HueError};
use super::session::Session;
use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(20);
const MAX_OUTPUT_DATAGRAMS: usize = 64;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_HANDSHAKE_INPUTS: usize = 4_096;
const MAX_SERVICE_INPUTS: usize = 32;

// Private adapter seam for deterministic tests; production always uses connected,
// nonblocking UDP. The adapter limits retained output, not all upstream allocations.
trait DatagramIo {
    fn receive(&mut self, target: &mut [u8]) -> io::Result<usize>;
    fn send(&mut self, data: &[u8]) -> io::Result<usize>;
    fn close(&mut self);
}

pub struct SocketIo(Option<UdpSocket>);

impl DatagramIo for SocketIo {
    fn receive(&mut self, target: &mut [u8]) -> io::Result<usize> {
        self.0.as_ref().ok_or(ErrorKind::NotConnected)?.recv(target)
    }
    fn send(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.as_ref().ok_or(ErrorKind::NotConnected)?.send(data)
    }
    fn close(&mut self) {
        self.0.take();
    }
}

#[allow(private_bounds)]
pub struct Client<I: DatagramIo = SocketIo> {
    io: Option<I>,
    session: Option<Session>,
    pending: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    handshake_inputs: usize,
}

fn check(deadline: Instant, cancel: &Cancellation) -> Result<(), HueError> {
    if cancel.cancelled() {
        return Err(HueError::Closed);
    }
    if Instant::now() >= deadline {
        return Err(HueError::Timeout);
    }
    Ok(())
}

impl Client {
    pub fn connect(
        config: HueConfig,
        cancel: Arc<Cancellation>,
        deadline: Instant,
    ) -> Result<Self, HueError> {
        check(deadline, &cancel)?;
        let ip = if config.peer.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        let socket = UdpSocket::bind(SocketAddr::new(ip, 0)).map_err(HueError::Io)?;
        socket.connect(config.peer).map_err(HueError::Io)?;
        socket.set_nonblocking(true).map_err(HueError::Io)?;
        let local = socket.local_addr().map_err(HueError::Io)?;
        Self::connect_with(config, SocketIo(Some(socket)), local, cancel, deadline)
    }
}

#[allow(private_bounds)]
impl<I: DatagramIo> Client<I> {
    fn connect_with(
        config: HueConfig,
        io: I,
        local: SocketAddr,
        cancel: Arc<Cancellation>,
        deadline: Instant,
    ) -> Result<Self, HueError> {
        let mut client = Self {
            io: Some(io),
            session: None,
            pending: VecDeque::new(),
            queued_bytes: 0,
            handshake_inputs: 0,
        };
        let result = (|| {
            check(deadline, &cancel)?;
            let mut session = Session::new(&config, local)?;
            session.start(Instant::now())?;
            client.session = Some(session);
            while !client.connected() {
                check(deadline, &cancel)?;
                client.service_ready(Instant::now(), &cancel, Some(deadline))?;
                client.flush(deadline, &cancel)?;
                if !client.connected() {
                    client.wait(deadline, &cancel)?;
                }
            }
            check(deadline, &cancel)?;
            client.flush(deadline, &cancel)
        })();
        if let Err(error) = result {
            client.dispose();
            return Err(error);
        }
        Ok(client)
    }

    pub fn connected(&self) -> bool {
        self.session.as_ref().is_some_and(Session::connected)
    }
    pub fn closed(&self) -> bool {
        self.io.is_none()
    }

    fn dispose(&mut self) {
        self.session.take();
        if let Some(mut io) = self.io.take() {
            io.close();
        }
        self.pending.clear();
        self.queued_bytes = 0;
    }

    // Reject before retaining the datagram and release every queued byte on failure.
    fn enqueue(&mut self, data: Vec<u8>) -> Result<(), HueError> {
        if self.pending.len() >= MAX_OUTPUT_DATAGRAMS
            || data.len() > MAX_OUTPUT_BYTES.saturating_sub(self.queued_bytes)
        {
            self.dispose();
            return Err(HueError::Protocol("transmit queue overflow"));
        }
        self.queued_bytes += data.len();
        self.pending.push_back(data);
        Ok(())
    }

    fn collect_output(&mut self) -> Result<(), HueError> {
        while let Some(data) = self
            .session
            .as_mut()
            .ok_or(HueError::Closed)?
            .poll_transmit()
        {
            self.enqueue(data)?;
        }
        Ok(())
    }

    fn flush_ready(
        &mut self,
        cancel: &Cancellation,
        deadline: Option<Instant>,
    ) -> Result<(), HueError> {
        while let Some(data) = self.pending.front() {
            if cancel.cancelled() {
                return Err(HueError::Closed);
            }
            if let Some(deadline) = deadline {
                check(deadline, cancel)?;
            }
            match self.io.as_mut().ok_or(HueError::Closed)?.send(data) {
                Ok(size) if size == data.len() => {
                    self.queued_bytes -= self.pending.pop_front().unwrap().len();
                }
                Ok(_) => return Err(HueError::Io(ErrorKind::WriteZero.into())),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => break,
                Err(error) => return Err(HueError::Io(error)),
            }
        }
        Ok(())
    }

    fn wait(&self, deadline: Instant, cancel: &Cancellation) -> Result<(), HueError> {
        check(deadline, cancel)?;
        let now = Instant::now();
        let until = self
            .session
            .as_ref()
            .and_then(Session::next_timeout)
            .unwrap_or(deadline)
            .min(deadline)
            .min(now + POLL);
        cancel.wait(until)
    }

    fn flush(&mut self, deadline: Instant, cancel: &Cancellation) -> Result<(), HueError> {
        self.collect_output()?;
        while !self.pending.is_empty() {
            check(deadline, cancel)?;
            let now = Instant::now();
            let session = self.session.as_mut().ok_or(HueError::Closed)?;
            if session.next_timeout().is_some_and(|timeout| now >= timeout) {
                session.timeout(now)?;
                self.collect_output()?;
            }
            self.flush_ready(cancel, Some(deadline))?;
            if !self.pending.is_empty() {
                self.wait(deadline, cancel)?;
            }
        }
        check(deadline, cancel)
    }

    pub fn service(&mut self, now: Instant, cancel: &Cancellation) -> Result<(), HueError> {
        let result = self.service_ready(now, cancel, None);
        if result.is_err() {
            self.dispose();
        }
        result
    }

    fn service_ready(
        &mut self,
        now: Instant,
        cancel: &Cancellation,
        deadline: Option<Instant>,
    ) -> Result<(), HueError> {
        if cancel.cancelled() {
            return Err(HueError::Closed);
        }
        if let Some(deadline) = deadline {
            check(deadline, cancel)?;
        }
        let session = self.session.as_mut().ok_or(HueError::Closed)?;
        if session.next_timeout().is_some_and(|timeout| now >= timeout) {
            session.timeout(now)?;
        }
        self.collect_output()?;
        let mut buffer = [0u8; 65_535];
        for _ in 0..MAX_SERVICE_INPUTS {
            if cancel.cancelled() {
                return Err(HueError::Closed);
            }
            if let Some(deadline) = deadline {
                check(deadline, cancel)?;
            }
            match self
                .io
                .as_mut()
                .ok_or(HueError::Closed)?
                .receive(&mut buffer)
            {
                Ok(size) => {
                    // Bound inputs to upstream fragment/cache/future-epoch queues
                    // throughout the candidate handshake, including duplicate traffic.
                    if !self.connected() {
                        if self.handshake_inputs >= MAX_HANDSHAKE_INPUTS {
                            return Err(HueError::Protocol("handshake input budget exhausted"));
                        }
                        self.handshake_inputs += 1;
                    }
                    self.session
                        .as_mut()
                        .ok_or(HueError::Closed)?
                        .read(Instant::now(), buffer[..size].to_vec())?;
                    self.collect_output()?;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(HueError::Io(error)),
            }
        }
        self.flush_ready(cancel, deadline)
    }

    pub fn send(
        &mut self,
        payload: &[u8],
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<(), HueError> {
        let result = (|| {
            check(deadline, cancel)?;
            self.service_ready(Instant::now(), cancel, Some(deadline))?;
            check(deadline, cancel)?;
            self.session
                .as_mut()
                .ok_or(HueError::Closed)?
                .write(Instant::now(), payload)?;
            self.flush(deadline, cancel)
        })();
        if result.is_err() {
            self.dispose();
        }
        result
    }

    pub fn close(&mut self, deadline: Instant) -> Result<(), HueError> {
        if self.closed() {
            return Ok(());
        }
        let result = (|| {
            self.session
                .as_mut()
                .ok_or(HueError::Closed)?
                .close(Instant::now())?;
            self.flush(deadline, &Cancellation::new())
        })();
        self.dispose();
        result
    }
}

impl<I: DatagramIo> Drop for Client<I> {
    fn drop(&mut self) {
        self.dispose();
    }
}

#[cfg(test)]
#[path = "../../tests/hue.rs"]
mod tests;
