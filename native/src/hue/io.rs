use super::config::{Cancellation, HueConfig, HueError};
use super::session::Session;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(20);

pub struct Client {
    socket: UdpSocket,
    session: Session,
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
        let socket = UdpSocket::bind(SocketAddr::new(ip, 0)).map_err(|_| HueError::Io)?;
        socket.connect(config.peer).map_err(|_| HueError::Io)?;
        socket.set_nonblocking(true).map_err(|_| HueError::Io)?;
        let mut session = Session::new(&config, socket.local_addr().map_err(|_| HueError::Io)?)?;
        session.start(Instant::now())?;
        let mut client = Self { socket, session };
        while !client.session.connected() {
            check(deadline, &cancel)?;
            client.flush(deadline, &cancel)?;
            check(deadline, &cancel)?;
            let mut buffer = [0u8; 65536];
            match client.socket.recv(&mut buffer) {
                Ok(size) => client
                    .session
                    .read(Instant::now(), buffer[..size].to_vec())?,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    let now = Instant::now();
                    if client
                        .session
                        .next_timeout()
                        .is_some_and(|timeout| now >= timeout)
                    {
                        client.session.timeout(now)?;
                    } else {
                        let until = client
                            .session
                            .next_timeout()
                            .unwrap_or(deadline)
                            .min(deadline)
                            .min(now + POLL);
                        cancel.wait(until)?;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(HueError::Io),
            }
        }
        check(deadline, &cancel)?;
        client.flush(deadline, &cancel)?;
        Ok(client)
    }

    pub fn send(
        &mut self,
        payload: &[u8],
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<(), HueError> {
        check(deadline, cancel)?;
        self.session.write(Instant::now(), payload)?;
        self.flush(deadline, cancel)
    }

    fn flush(&mut self, deadline: Instant, cancel: &Cancellation) -> Result<(), HueError> {
        while let Some(datagram) = self.session.poll_transmit() {
            loop {
                check(deadline, cancel)?;
                match self.socket.send(&datagram) {
                    Ok(size) if size == datagram.len() => break,
                    Ok(_) => return Err(HueError::Io),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        cancel.wait(deadline.min(Instant::now() + POLL))?;
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => return Err(HueError::Io),
                }
            }
        }
        Ok(())
    }
}
