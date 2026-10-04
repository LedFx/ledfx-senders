//! One synchronous state machine for recording, discard and real UDP backends.
use crate::buffer::Banks;
use crate::transport::{Datagram, DatagramTransport, MemoryTransport, SocketTransport};
use std::{
    io,
    net::SocketAddrV4,
    time::{Duration, Instant},
};

pub enum Transport {
    Socket(SocketTransport),
    Memory(MemoryTransport),
}
impl DatagramTransport for Transport {
    fn send_batch(&mut self, p: &[Datagram<'_>], d: Instant) -> io::Result<usize> {
        match self {
            Self::Socket(t) => t.send_batch(p, d),
            Self::Memory(t) => t.send_batch(p, d),
        }
    }
    fn close(&mut self) {
        match self {
            Self::Socket(t) => t.close(),
            Self::Memory(t) => t.close(),
        }
    }
}
pub struct Sender<T: DatagramTransport> {
    pub banks: Banks,
    pub transport: T,
    pub closed: bool,
    destinations: Vec<SocketAddrV4>,
    sync_destination: SocketAddrV4,
    discovery_destination: SocketAddrV4,
    sync: Vec<u8>,
    discovery: Vec<Vec<u8>>,
    sequences: Vec<u8>,
    sync_sequence: u8,
    used: Vec<bool>,
    committed: bool,
    native_clock: Box<dyn Fn() -> Instant + Send>,
    last_data: Option<f64>,
    last_discovery: Option<f64>,
    pub datagrams: u64,
    pub bytes: u64,
    pub errors: u64,
    pub cleanup_error: Option<String>,
}
impl<T: DatagramTransport> Sender<T> {
    pub fn new(
        banks: Banks,
        transport: T,
        destinations: Vec<SocketAddrV4>,
        sync: Vec<u8>,
        discovery: Vec<Vec<u8>>,
        override_destination: Option<SocketAddrV4>,
    ) -> io::Result<Self> {
        if destinations.len() != banks.committed.len()
            || sync.len() != 49
            || discovery.is_empty()
            || discovery.len() > 128
            || discovery.iter().any(|p| p.len() < 120)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid sender templates",
            ));
        }
        let n = destinations.len();
        Ok(Self {
            native_clock: Box::new(Instant::now),
            banks,
            transport,
            closed: false,
            destinations,
            sync_destination: override_destination
                .unwrap_or_else(|| "239.255.249.255:5568".parse().unwrap()),
            discovery_destination: override_destination
                .unwrap_or_else(|| "239.255.250.214:5568".parse().unwrap()),
            sync,
            discovery,
            sequences: vec![0; n],
            sync_sequence: 0,
            used: vec![false; n],
            committed: false,
            last_data: None,
            last_discovery: None,
            datagrams: 0,
            bytes: 0,
            errors: 0,
            cleanup_error: None,
        })
    }
    fn transmit(&mut self, staging: bool, termination: bool, deadline: Instant) -> io::Result<()> {
        let packets = if staging {
            &mut self.banks.staging
        } else {
            &mut self.banks.committed
        };
        for (i, p) in packets.iter_mut().enumerate() {
            p[111] = self.sequences[i];
            p[112] = if termination { 0x40 } else { 0 };
        }
        // Bounded stack descriptors avoid per-frame heap allocation. Each
        // chunk remains borrowed from the bank until all accepted suffixes finish.
        let mut next = 0;
        while next < packets.len() {
            let mut indices = [0usize; 1024];
            let mut datagrams: [Datagram<'_>; 1024] = std::array::from_fn(|_| Datagram {
                bytes: &[],
                destination: self.sync_destination,
            });
            let mut count = 0;
            while next < packets.len() && count < 1024 {
                let i = next;
                next += 1;
                if (termination || self.closed) && !self.used[i] {
                    continue;
                }
                indices[count] = i;
                datagrams[count] = Datagram {
                    bytes: &packets[i],
                    destination: self.destinations[i],
                };
                count += 1;
            }
            let mut offset = 0;
            while offset < count {
                if (self.native_clock)() >= deadline {
                    self.errors += 1;
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "sender deadline"));
                }
                match self
                    .transport
                    .send_batch(&datagrams[offset..count], deadline)
                {
                    Ok(n) if n > 0 && n <= count - offset => {
                        for (d, &i) in datagrams[offset..offset + n]
                            .iter()
                            .zip(&indices[offset..offset + n])
                        {
                            self.datagrams += 1;
                            self.bytes += d.bytes.len() as u64;
                            self.sequences[i] = self.sequences[i].wrapping_add(1);
                            self.used[i] = true;
                        }
                        offset += n;
                    }
                    Ok(_) => {
                        self.errors += 1;
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "invalid accepted prefix",
                        ));
                    }
                    Err(e) => {
                        self.errors += 1;
                        return Err(e);
                    }
                }
            }
        }
        if termination {
            return Ok(());
        }
        if (self.native_clock)() >= deadline {
            self.errors += 1;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "sync deadline"));
        }
        self.sync[44] = self.sync_sequence;
        let d = Datagram {
            bytes: &self.sync,
            destination: self.sync_destination,
        };
        match self.transport.send_batch(&[d], deadline) {
            Ok(1) => {
                self.datagrams += 1;
                self.bytes += 49;
                self.sync_sequence = self.sync_sequence.wrapping_add(1);
                Ok(())
            }
            Ok(_) => {
                self.errors += 1;
                Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "sync not accepted",
                ))
            }
            Err(e) => {
                self.errors += 1;
                Err(e)
            }
        }
    }
    fn announce(&mut self, now: f64, deadline: Instant) -> io::Result<()> {
        if self.last_discovery.is_some_and(|last| now - last < 10.0) {
            return Ok(());
        }
        let mut storage: [Datagram<'_>; 128] = std::array::from_fn(|_| Datagram {
            bytes: &[],
            destination: self.discovery_destination,
        });
        for (d, p) in storage.iter_mut().zip(&self.discovery) {
            *d = Datagram {
                bytes: p,
                destination: self.discovery_destination,
            };
        }
        let packets = &storage[..self.discovery.len()];
        let mut offset = 0;
        while offset < packets.len() {
            if (self.native_clock)() >= deadline {
                self.errors += 1;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "discovery deadline",
                ));
            }
            match self.transport.send_batch(&packets[offset..], deadline) {
                Ok(n) if n > 0 && n <= packets.len() - offset => {
                    self.datagrams += n as u64;
                    self.bytes += packets[offset..offset + n]
                        .iter()
                        .map(|p| p.bytes.len() as u64)
                        .sum::<u64>();
                    offset += n;
                }
                Ok(_) => {
                    self.errors += 1;
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "discovery not accepted",
                    ));
                }
                Err(e) => {
                    self.errors += 1;
                    return Err(e);
                }
            }
        }
        self.last_discovery = Some(now);
        Ok(())
    }
    pub fn send_prepared(&mut self, kind: u8, now: f64) -> io::Result<()> {
        if self.closed {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "sender closed"));
        }
        self.banks
            .prepare(kind)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let deadline = (self.native_clock)() + Duration::from_millis(200);
        self.transmit(true, false, deadline)?;
        self.banks.commit();
        self.committed = true;
        self.last_data = Some(now);
        Ok(())
    }
    pub fn service(&mut self, now: f64) -> io::Result<()> {
        // Maintenance never advertises or refreshes an uninitialized source.
        if self.closed || !self.committed {
            return Ok(());
        }
        let deadline = (self.native_clock)() + Duration::from_millis(200);
        self.announce(now, deadline)?;
        if self.committed && self.last_data.is_some_and(|last| now - last >= 0.8) {
            self.transmit(false, false, deadline)?;
            self.last_data = Some(now);
        }
        Ok(())
    }
    pub fn close(&mut self, blackout: bool, _now: f64) {
        if self.closed {
            return;
        }
        self.closed = true;
        let deadline = (self.native_clock)() + Duration::from_secs(1);
        if self.used.iter().any(|&u| u) {
            if blackout {
                for p in &mut self.banks.committed {
                    p[126..].fill(0);
                }
                if let Err(e) = self.transmit(false, false, deadline) {
                    self.cleanup_error = Some(e.to_string());
                }
            }
            for _ in 0..3 {
                if (self.native_clock)() >= deadline {
                    if self.cleanup_error.is_none() {
                        self.cleanup_error = Some("shutdown deadline".to_owned());
                    }
                    break;
                }
                if let Err(e) = self.transmit(false, true, deadline)
                    && self.cleanup_error.is_none()
                {
                    self.cleanup_error = Some(e.to_string());
                }
            }
        }
        self.transport.close();
    }
}

#[cfg(test)]
#[path = "../tests/sender.rs"]
mod tests;
