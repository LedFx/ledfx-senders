//! Fixed-capacity native receive storage. Portable fallback and Linux batching.
#[cfg(not(unix))]
use std::time::Duration;
use std::{
    io,
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
};

pub struct Receiver {
    socket: UdpSocket,
    portable: Vec<u8>,
    pub backend: &'static str,
    pub calls: u64,
    #[cfg(target_os = "linux")]
    batch: Option<Batch>,
}
#[cfg(target_os = "linux")]
struct Batch {
    buffers: Vec<Vec<u8>>,
    addresses: Vec<libc::sockaddr_in>,
    iovecs: Vec<libc::iovec>,
    headers: Vec<libc::mmsghdr>,
}
#[cfg(target_os = "linux")]
impl Batch {
    fn new() -> Self {
        // All syscall fields have valid zero/null representations. Pointers are
        // filled immediately before each call and cleared before returning.
        Self {
            buffers: (0..64).map(|_| vec![0; 65535]).collect(),
            addresses: (0..64).map(|_| unsafe { std::mem::zeroed() }).collect(),
            iovecs: (0..64).map(|_| unsafe { std::mem::zeroed() }).collect(),
            headers: (0..64).map(|_| unsafe { std::mem::zeroed() }).collect(),
        }
    }
}
impl Receiver {
    pub fn new(batched: bool) -> io::Result<Self> {
        Self::bind(batched, Ipv4Addr::LOCALHOST)
    }
    pub fn bind(batched: bool, address: Ipv4Addr) -> io::Result<Self> {
        if !address.is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "receiver requires loopback",
            ));
        }
        let socket = UdpSocket::bind((address, 0))?;
        socket.set_nonblocking(true)?;
        socket2::SockRef::from(&socket).set_recv_buffer_size(16 * 1024 * 1024)?;
        Ok(Self {
            socket,
            portable: vec![0; 65535],
            backend: if batched && cfg!(target_os = "linux") {
                "recvmmsg"
            } else {
                "portable"
            },
            calls: 0,
            #[cfg(target_os = "linux")]
            batch: batched.then(Batch::new),
        })
    }
    pub fn port(&self) -> io::Result<u16> {
        Ok(self.socket.local_addr()?.port())
    }
    pub fn buffer_size(&self) -> io::Result<usize> {
        socket2::SockRef::from(&self.socket).recv_buffer_size()
    }
    pub fn kernel_drops(&self) -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let link =
                std::fs::read_link(format!("/proc/self/fd/{}", self.socket.as_raw_fd())).ok()?;
            let link = link.to_str()?;
            let inode = link.strip_prefix("socket:[")?.strip_suffix(']')?;
            let table = std::fs::read_to_string("/proc/self/net/udp").ok()?;
            for line in table.lines().skip(1) {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.get(9) == Some(&inode) {
                    return fields.last()?.parse().ok();
                }
            }
            None
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
    pub fn wait(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut poll = libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized descriptor is valid throughout the call.
            let n = unsafe { libc::poll(&mut poll, 1, 5) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
        #[cfg(not(unix))]
        std::thread::sleep(Duration::from_millis(1));
        Ok(())
    }
    pub fn receive(&mut self, mut feed: impl FnMut(&[u8], SocketAddrV4)) -> io::Result<usize> {
        self.calls += 1;
        #[cfg(target_os = "linux")]
        if let Some(batch) = &mut self.batch {
            use std::os::fd::AsRawFd;
            for i in 0..64 {
                batch.iovecs[i] = libc::iovec {
                    iov_base: batch.buffers[i].as_mut_ptr().cast(),
                    iov_len: 65535,
                };
                // SAFETY: optional pointers are nullable; every required field
                // is then supplied from stable heap allocations owned by batch.
                batch.headers[i] = unsafe { std::mem::zeroed() };
                batch.headers[i].msg_hdr.msg_name =
                    (&mut batch.addresses[i] as *mut libc::sockaddr_in).cast();
                batch.headers[i].msg_hdr.msg_namelen =
                    std::mem::size_of::<libc::sockaddr_in>() as _;
                batch.headers[i].msg_hdr.msg_iov = &mut batch.iovecs[i];
                batch.headers[i].msg_hdr.msg_iovlen = 1;
            }
            // SAFETY: all 64 writable payload buffers/descriptors are alive and
            // exclusively borrowed until this synchronous nonblocking call ends.
            let result = unsafe {
                libc::recvmmsg(
                    self.socket.as_raw_fd(),
                    batch.headers.as_mut_ptr(),
                    64,
                    libc::MSG_DONTWAIT,
                    std::ptr::null_mut(),
                )
            };
            let error = if result < 0 {
                Some(io::Error::last_os_error())
            } else {
                None
            };
            for i in 0..64 {
                batch.headers[i].msg_hdr.msg_name = std::ptr::null_mut();
                batch.headers[i].msg_hdr.msg_iov = std::ptr::null_mut();
                batch.iovecs[i].iov_base = std::ptr::null_mut();
            }
            if let Some(error) = error {
                return Err(error);
            }
            for i in 0..result as usize {
                let len = batch.headers[i].msg_len as usize;
                if len > 65535 || batch.headers[i].msg_hdr.msg_flags & libc::MSG_TRUNC != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "truncated UDP datagram",
                    ));
                }
                let address = batch.addresses[i];
                if address.sin_family != libc::AF_INET as _ {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected address family",
                    ));
                }
                feed(
                    &batch.buffers[i][..len],
                    SocketAddrV4::new(
                        Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()),
                        u16::from_be(address.sin_port),
                    ),
                );
            }
            return Ok(result as usize);
        }
        let (len, source) = self.socket.recv_from(&mut self.portable)?;
        let std::net::SocketAddr::V4(source) = source else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected IPv6 peer",
            ));
        };
        feed(&self.portable[..len], source);
        Ok(1)
    }
}
