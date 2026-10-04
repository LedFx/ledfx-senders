//! Protocol-independent, deadline-bounded UDP transport.
use std::io;
use std::net::{SocketAddrV4, UdpSocket};
use std::time::Instant;

pub struct Datagram<'a> {
    pub bytes: &'a [u8],
    pub destination: SocketAddrV4,
}
pub trait DatagramTransport: Send {
    /// Errors accept zero; a failure after progress returns the accepted prefix.
    fn send_batch(&mut self, packets: &[Datagram<'_>], deadline: Instant) -> io::Result<usize>;
    fn close(&mut self);
}

#[cfg(target_os = "linux")]
struct BatchStorage {
    addresses: Vec<libc::sockaddr_in>,
    iovecs: Vec<libc::iovec>,
    headers: Vec<libc::mmsghdr>,
}
// SAFETY: pointer fields are cleared after each synchronous syscall. Access is
// exclusive through RefCell under the engine mutex; nothing dereferences stored
// descriptors outside linux_batch, and no descriptor escapes this module.
#[cfg(target_os = "linux")]
unsafe impl Send for BatchStorage {}

pub struct SocketTransport {
    socket: Option<UdpSocket>,
    pub batched: bool,
    pub batch_size: usize,
    pub syscalls: std::cell::Cell<u64>,
    pub readiness_waits: std::cell::Cell<u64>,
    #[cfg(target_os = "linux")]
    storage: std::cell::RefCell<BatchStorage>,
}
impl SocketTransport {
    /// Test seam: all multicast uses the loopback interface, never the LAN.
    pub fn loopback_multicast(&self) -> io::Result<()> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "socket closed"))?;
        socket2::SockRef::from(socket).set_multicast_if_v4(&std::net::Ipv4Addr::LOCALHOST)?;
        socket.set_multicast_loop_v4(true)
    }

    pub fn new(batched: bool, batch_size: usize) -> io::Result<Self> {
        if batch_size == 0 || batch_size > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid batch size",
            ));
        }
        let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_nonblocking(true)?;
        socket.set_multicast_ttl_v4(1)?;
        Ok(Self {
            socket: Some(socket),
            batched,
            batch_size,
            syscalls: std::cell::Cell::new(0),
            readiness_waits: std::cell::Cell::new(0),
            #[cfg(target_os = "linux")]
            storage: std::cell::RefCell::new(BatchStorage {
                // SAFETY: all-zero sockaddr/iovec/msghdr have valid scalar and
                // null pointer representations. Fields are filled before use.
                addresses: (0..batch_size)
                    .map(|_| unsafe { std::mem::zeroed() })
                    .collect(),
                iovecs: (0..batch_size)
                    .map(|_| unsafe { std::mem::zeroed() })
                    .collect(),
                headers: (0..batch_size)
                    .map(|_| unsafe { std::mem::zeroed() })
                    .collect(),
            }),
        })
    }
    fn wait(&self, deadline: Instant) -> io::Result<()> {
        self.readiness_waits.set(self.readiness_waits.get() + 1);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "UDP deadline"));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut fd = libc::pollfd {
                fd: self.socket.as_ref().unwrap().as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let timeout = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
            // SAFETY: fd points to one initialized pollfd for the duration of poll.
            let result = unsafe { libc::poll(&mut fd, 1, timeout) };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            if result == 0 {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "UDP deadline"));
            }
        }
        #[cfg(not(unix))]
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(1)));
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn linux_batch(&self, packets: &[Datagram<'_>]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;
        let mut storage = self.storage.borrow_mut();
        let BatchStorage {
            addresses,
            iovecs,
            headers,
        } = &mut *storage;
        for (i, p) in packets.iter().enumerate() {
            addresses[i] = libc::sockaddr_in {
                sin_family: libc::AF_INET as _,
                sin_port: p.destination.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(p.destination.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            iovecs[i] = libc::iovec {
                iov_base: p.bytes.as_ptr() as *mut _,
                iov_len: p.bytes.len(),
            };
            // SAFETY: null optional pointers and zero scalar fields are valid.
            headers[i] = unsafe { std::mem::zeroed() };
            headers[i].msg_hdr.msg_name = &mut addresses[i] as *mut _ as *mut _;
            headers[i].msg_hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as _;
            headers[i].msg_hdr.msg_iov = &mut iovecs[i];
            headers[i].msg_hdr.msg_iovlen = 1;
        }
        // SAFETY: headers, addresses, iovecs and borrowed packet bytes are stable
        // through this synchronous call; kernel only reads borrowed payload bytes.
        let result = unsafe {
            libc::sendmmsg(
                self.socket.as_ref().unwrap().as_raw_fd(),
                headers.as_mut_ptr(),
                packets.len() as _,
                libc::MSG_DONTWAIT,
            )
        };
        let error = if result < 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        // Do not retain borrowed payload addresses or self-relative pointers.
        for i in 0..packets.len() {
            iovecs[i].iov_base = std::ptr::null_mut();
            headers[i].msg_hdr.msg_name = std::ptr::null_mut();
            headers[i].msg_hdr.msg_iov = std::ptr::null_mut();
        }
        if let Some(error) = error {
            return Err(error);
        }
        for (i, (h, p)) in headers
            .iter()
            .zip(packets)
            .take(result as usize)
            .enumerate()
        {
            if h.msg_len as usize != p.bytes.len() {
                return if i > 0 {
                    Ok(i)
                } else {
                    Err(io::Error::new(io::ErrorKind::WriteZero, "short UDP write"))
                };
            }
        }
        Ok(result as usize)
    }
}
impl DatagramTransport for SocketTransport {
    fn send_batch(&mut self, packets: &[Datagram<'_>], deadline: Instant) -> io::Result<usize> {
        if self.socket.is_none() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "closed socket"));
        }
        drive_batch(
            packets.len(),
            self.batch_size,
            deadline,
            Instant::now,
            |offset, end| {
                self.syscalls.set(self.syscalls.get() + 1);
                #[cfg(target_os = "linux")]
                if self.batched {
                    return self.linux_batch(&packets[offset..end]);
                }
                #[cfg(not(target_os = "linux"))]
                let _ = end;
                let p = &packets[offset];
                complete_datagram(
                    self.socket
                        .as_ref()
                        .unwrap()
                        .send_to(p.bytes, p.destination)?,
                    p.bytes.len(),
                )
            },
            |deadline| self.wait(deadline),
        )
    }

    fn close(&mut self) {
        self.socket.take();
    }
}

pub struct MemoryTransport {
    pub capture: bool,
    pub packets: Vec<(Vec<u8>, String)>,
}
impl DatagramTransport for MemoryTransport {
    fn send_batch(&mut self, packets: &[Datagram<'_>], _: Instant) -> io::Result<usize> {
        if self.capture {
            self.packets.extend(
                packets
                    .iter()
                    .map(|p| (p.bytes.to_vec(), p.destination.to_string())),
            );
        }
        Ok(packets.len())
    }
    fn close(&mut self) {}
}

fn complete_datagram(written: usize, expected: usize) -> io::Result<usize> {
    if written == expected {
        Ok(1)
    } else {
        Err(io::Error::new(io::ErrorKind::WriteZero, "short UDP write"))
    }
}
/// Shared retry policy, with injectable syscall/readiness/monotonic clock for tests.
fn drive_batch(
    count: usize,
    batch_size: usize,
    deadline: Instant,
    now: impl Fn() -> Instant,
    mut submit: impl FnMut(usize, usize) -> io::Result<usize>,
    mut wait: impl FnMut(Instant) -> io::Result<()>,
) -> io::Result<usize> {
    let mut accepted = 0;
    while accepted < count {
        if now() >= deadline {
            return if accepted > 0 {
                Ok(accepted)
            } else {
                Err(io::Error::new(io::ErrorKind::TimedOut, "UDP deadline"))
            };
        }
        let end = (accepted + batch_size).min(count);
        let result = match submit(accepted, end) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => match wait(deadline) {
                Ok(()) => continue,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => Err(e),
            },
            result => result,
        };
        match result {
            Ok(n) if n > 0 && n <= end - accepted => accepted += n,
            Ok(_) => {
                return if accepted > 0 {
                    Ok(accepted)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "invalid UDP progress",
                    ))
                };
            }
            Err(e) => return if accepted > 0 { Ok(accepted) } else { Err(e) },
        }
    }
    Ok(accepted)
}
#[cfg(test)]
#[path = "../tests/transport.rs"]
mod tests;
