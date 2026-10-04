//! Independent benchmark wire oracle. It imports no production packet encoder.
use std::{
    borrow::Cow,
    time::{Duration, Instant},
};
#[path = "stateful.rs"]
mod stateful;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Ddp,
    E131,
    Opc,
    OscOne,
    OscThree,
    OscChannels,
    OscAll,
    Drgb,
    Warls,
    Drgbw,
    Dnrgb,
    Raw,
    Adaptive,
}
impl Protocol {
    pub fn parse(s: &str) -> Result<Self, &'static str> {
        match s {
            "ddp" => Ok(Self::Ddp),
            "e131" => Ok(Self::E131),
            "opc" => Ok(Self::Opc),
            "osc-one" => Ok(Self::OscOne),
            "osc-three" => Ok(Self::OscThree),
            "osc-channels" => Ok(Self::OscChannels),
            "osc-all" => Ok(Self::OscAll),
            "udp-drgb" => Ok(Self::Drgb),
            "udp-warls" => Ok(Self::Warls),
            "udp-drgbw" => Ok(Self::Drgbw),
            "udp-dnrgb" => Ok(Self::Dnrgb),
            "udp-raw" => Ok(Self::Raw),
            "udp-adaptive" => Ok(Self::Adaptive),
            _ => Err("unsupported protocol"),
        }
    }
    fn osc(self) -> bool {
        matches!(
            self,
            Self::OscOne | Self::OscThree | Self::OscChannels | Self::OscAll
        )
    }
    fn realtime(self) -> bool {
        matches!(
            self,
            Self::Drgb | Self::Warls | Self::Drgbw | Self::Dnrgb | Self::Raw | Self::Adaptive
        )
    }
    fn realtime_kind(self, count: usize) -> u8 {
        match self {
            Self::Drgb if count <= 1470 => 2,
            Self::Warls if count <= 765 => 1,
            Self::Drgbw if count <= 1101 => 3,
            Self::Raw if count <= 1500 => 0,
            Self::Dnrgb => 4,
            Self::Adaptive if count <= 765 => 2,
            _ => {
                if count <= 1470 {
                    2
                } else {
                    4
                }
            }
        }
    }
    pub fn chunk_size(self, count: usize) -> usize {
        match self {
            Self::Ddp => 1440,
            Self::E131 => 510,
            Self::Opc | Self::OscAll => count,
            Self::OscOne | Self::OscThree => 12,
            Self::OscChannels => 4,
            _ => {
                if self.realtime_kind(count) == 4 {
                    1467
                } else {
                    count
                }
            }
        }
    }
}
#[derive(Default)]
pub struct Counts {
    pub datagrams: u64,
    pub bytes: u64,
    pub complete: u64,
    pub incomplete: u64,
    pub invalid: u64,
    pub duplicates: u64,
    pub late: u64,
    pub reordered: u64,
    pub sync: u64,
    pub maintenance: u64,
    pub wrong_sequence: u64,
}
struct Frame {
    id: u64,
    seen: Vec<u64>,
    received: usize,
    complete: bool,
    expired: bool,
    last: Instant,
}
impl Frame {
    fn new(chunks: usize, now: Instant) -> Self {
        Self {
            id: 0,
            seen: vec![0; chunks.div_ceil(64)],
            received: 0,
            complete: false,
            expired: false,
            last: now,
        }
    }
    fn reset(&mut self, id: u64, now: Instant) {
        self.id = id;
        self.seen.fill(0);
        self.received = 0;
        self.complete = false;
        self.expired = false;
        self.last = now;
    }
}
struct Data<'a> {
    payload: Cow<'a, [u8]>,
    index: usize,
    sequence: Option<u8>,
}
enum Packet<'a> {
    Data(Data<'a>),
    Sync,
    Maintenance,
}

pub struct Oracle {
    protocol: Protocol,
    expected: Vec<u8>,
    identifier: u8,
    chunk_size: usize,
    chunks: usize,
    frames: Vec<Frame>,
    highest: u64,
    source_cid: Option<[u8; 16]>,
    osc_tags: Vec<u8>,
    pub max_identity: u64,
    pub counts: Counts,
}
impl Oracle {
    pub fn new(
        protocol: Protocol,
        expected: Vec<u8>,
        identifier: u8,
        window: usize,
        now: Instant,
    ) -> Result<Self, &'static str> {
        let count = expected.len();
        if count == 0 || count > 3_000_000 || !(2..=4096).contains(&window) {
            return Err("invalid channel count or reorder window");
        }
        if protocol == Protocol::Opc && count > 65503 {
            return Err("OPC exceeds UDP ceiling");
        }
        if protocol.osc() && !count.is_multiple_of(12)
            || protocol.realtime() && (!count.is_multiple_of(3) || count > 65536 * 3)
        {
            return Err("invalid stateful fixture RGB length");
        }
        let osc_tags = stateful::tags(protocol, count);
        let chunk_size = protocol.chunk_size(count);
        let chunks = count.div_ceil(chunk_size);
        let smallest = (count - (chunks - 1) * chunk_size).min(8);
        if smallest < 3 {
            return Err("benchmark requires at least three identity bytes in every chunk");
        }
        let max_identity = if protocol.osc() {
            1 << 24
        } else if protocol.realtime() {
            (1 << 24) - 1
        } else if smallest == 8 {
            u64::MAX
        } else {
            (1u64 << (smallest * 8)) - 1
        };
        Ok(Self {
            protocol,
            expected,
            identifier,
            chunk_size,
            chunks,
            frames: (0..window).map(|_| Frame::new(chunks, now)).collect(),
            highest: 0,
            source_cid: None,
            osc_tags,
            max_identity,
            counts: Counts::default(),
        })
    }
    fn parse<'a>(&self, p: &'a [u8]) -> Result<Packet<'a>, ()> {
        let count = self.expected.len();
        if self.protocol.osc() {
            return stateful::osc(self.protocol, p, count, &self.osc_tags).map(Packet::Data);
        }
        if self.protocol.realtime() {
            return stateful::realtime(self.protocol, p, &self.expected, self.identifier)
                .map(Packet::Data);
        }
        match self.protocol {
            Protocol::Ddp => {
                if p.len() < 10 || p[2] != 11 || p[3] != self.identifier {
                    return Err(());
                }
                let start = u32::from_be_bytes(p[4..8].try_into().unwrap()) as usize;
                if start >= count || !start.is_multiple_of(1440) {
                    return Err(());
                }
                let size = (count - start).min(1440);
                let flags = if start + size == count { 0x41 } else { 0x40 };
                if p[0] != flags
                    || p.len() != 10 + size
                    || u16::from_be_bytes(p[8..10].try_into().unwrap()) as usize != size
                {
                    return Err(());
                }
                Ok(Packet::Data(Data {
                    payload: Cow::Borrowed(&p[10..]),
                    index: start / 1440,
                    sequence: Some(p[1]),
                }))
            }
            Protocol::Opc => {
                if p.len() != count + 4
                    || p[0] != self.identifier
                    || p[1] != 0
                    || u16::from_be_bytes(p[2..4].try_into().unwrap()) as usize != count
                {
                    return Err(());
                }
                Ok(Packet::Data(Data {
                    payload: Cow::Borrowed(&p[4..]),
                    index: 0,
                    sequence: None,
                }))
            }
            Protocol::E131 => {
                if p.len() < 49
                    || p[..16] != [0, 16, 0, 0, 65, 83, 67, 45, 69, 49, 46, 49, 55, 0, 0, 0]
                {
                    return Err(());
                }
                for offset in [16, 38] {
                    if u16::from_be_bytes(p[offset..offset + 2].try_into().unwrap()) as usize
                        != (0x7000 | (p.len() - offset))
                    {
                        return Err(());
                    }
                }
                let root = u32::from_be_bytes(p[18..22].try_into().unwrap());
                let vector = u32::from_be_bytes(p[40..44].try_into().unwrap());
                if (root, vector) == (8, 1) {
                    if p.len() != 49
                        || u16::from_be_bytes(p[45..47].try_into().unwrap()) != 63999
                        || p[47..] != [0, 0]
                    {
                        return Err(());
                    }
                    return Ok(Packet::Sync);
                }
                if (root, vector) == (8, 2) {
                    if !(120..=1144).contains(&p.len())
                        || !p.len().is_multiple_of(2)
                        || p[108..112] != [0, 0, 0, 0]
                        || p[114..118] != [0, 0, 0, 1]
                        || p[118] > p[119]
                        || u16::from_be_bytes(p[112..114].try_into().unwrap()) as usize
                            != (0x7000 | (p.len() - 112))
                    {
                        return Err(());
                    }
                    let start = p[118] as usize * 512;
                    let expected_end = (start + 512).min(self.chunks);
                    if p[119] as usize != (self.chunks - 1) / 512
                        || start >= self.chunks
                        || p.len() != 120 + 2 * (expected_end - start)
                    {
                        return Err(());
                    }
                    for (i, u) in p[120..].chunks_exact(2).enumerate() {
                        if u16::from_be_bytes(u.try_into().unwrap()) as usize != start + i + 1 {
                            return Err(());
                        }
                    }
                    return Ok(Packet::Maintenance);
                }
                if (root, vector) != (4, 2)
                    || p.len() != 638
                    || p[108] > 200
                    || p[115..126] != [0x72, 0x0b, 2, 0xa1, 0, 0, 0, 1, 2, 1, 0]
                    || u16::from_be_bytes(p[109..111].try_into().unwrap()) != 63999
                {
                    return Err(());
                }
                let universe = u16::from_be_bytes(p[113..115].try_into().unwrap()) as usize;
                if universe == 0 || universe > self.chunks {
                    return Err(());
                }
                if p[112] == 0x40 {
                    return Ok(Packet::Maintenance);
                }
                if p[112] != 0 {
                    return Err(());
                }
                let universe = u16::from_be_bytes(p[113..115].try_into().unwrap()) as usize;
                if universe == 0 || universe > self.chunks {
                    return Err(());
                }
                let index = universe - 1;
                let used = (count - index * 510).min(510);
                if p[126 + used..].iter().any(|&v| v != 0) {
                    return Err(());
                }
                Ok(Packet::Data(Data {
                    payload: Cow::Borrowed(&p[126..126 + used]),
                    index,
                    sequence: Some(p[111]),
                }))
            }
            _ => unreachable!(),
        }
    }
    // A benchmark receiver observes one E131 source. Pin only after all wire,
    // fixture and sequence validation, before any frame assembly mutation.
    fn accept_source(&mut self, packet: &[u8]) -> bool {
        if self.protocol != Protocol::E131 {
            return true;
        }
        let cid: [u8; 16] = packet[22..38].try_into().unwrap();
        match self.source_cid {
            Some(expected) => cid == expected,
            None => {
                self.source_cid = Some(cid);
                true
            }
        }
    }
    pub fn feed(&mut self, packet: &[u8], now: Instant) {
        self.counts.datagrams += 1;
        self.counts.bytes += packet.len() as u64;
        let data = match self.parse(packet) {
            Ok(Packet::Data(data)) => data,
            Ok(Packet::Sync) => {
                if self.accept_source(packet) {
                    self.counts.sync += 1;
                } else {
                    self.counts.invalid += 1;
                }
                return;
            }
            Ok(Packet::Maintenance) => {
                if self.accept_source(packet) {
                    self.counts.maintenance += 1;
                } else {
                    self.counts.invalid += 1;
                }
                return;
            }
            Err(()) => {
                self.counts.invalid += 1;
                return;
            }
        };
        let marker = if self.protocol.osc() {
            4
        } else if self.protocol.realtime() {
            3
        } else {
            data.payload.len().min(8)
        };
        let id = if self.protocol.osc() {
            let value = f32::from_be_bytes(data.payload[..4].try_into().unwrap());
            if !value.is_finite() || !(1.0..=16777216.0).contains(&value) || value.fract() != 0.0 {
                self.counts.invalid += 1;
                return;
            }
            value as u64
        } else {
            data.payload[..marker]
                .iter()
                .fold(0u64, |n, &v| (n << 8) | v as u64)
        };
        let start = data.index * self.chunk_size;
        if id == 0
            || id > self.max_identity
            || data.payload[marker..] != self.expected[start + marker..start + data.payload.len()]
        {
            self.counts.invalid += 1;
            return;
        }
        let sequence = match self.protocol {
            Protocol::Ddp => Some((id % 15 + 1) as u8),
            Protocol::E131 => Some(((id - 1) % 256) as u8),
            _ => None,
        };
        if data.sequence != sequence {
            self.counts.wrong_sequence += 1;
            self.counts.invalid += 1;
            return;
        }
        if !self.accept_source(packet) {
            self.counts.invalid += 1;
            return;
        }
        let window = self.frames.len() as u64;
        if id.saturating_add(window) <= self.highest {
            self.counts.late += 1;
            return;
        }
        let old_highest = self.highest;
        self.highest = self.highest.max(id);
        let frame = &mut self.frames[(id % window) as usize];
        if frame.id != id {
            if frame.id > id {
                self.counts.late += 1;
                return;
            }
            if frame.id != 0 && !frame.complete && !frame.expired {
                self.counts.incomplete += 1;
            }
            frame.reset(id, now);
        }
        if frame.expired {
            self.counts.late += 1;
            return;
        }
        let bit = 1u64 << (data.index % 64);
        if frame.seen[data.index / 64] & bit != 0 {
            self.counts.duplicates += 1;
            return;
        }
        if id < old_highest || data.index != frame.received {
            self.counts.reordered += 1;
        }
        frame.seen[data.index / 64] |= bit;
        frame.received += 1;
        frame.last = now;
        if frame.received == self.chunks {
            frame.complete = true;
            self.counts.complete += 1;
        }
    }
    pub fn incomplete_sample(&self) -> String {
        let entries: Vec<_> = self
            .frames
            .iter()
            .filter(|f| f.id != 0 && !f.complete)
            .take(16)
            .map(|f| format!("[{},{}]", f.id, f.received))
            .collect();
        format!("[{}]", entries.join(","))
    }
    pub fn highest_identity(&self) -> u64 {
        self.highest
    }
    pub fn expire(&mut self, now: Instant, finish: bool) {
        for frame in &mut self.frames {
            if frame.id != 0
                && !frame.complete
                && !frame.expired
                && (finish || now.duration_since(frame.last) >= Duration::from_millis(500))
            {
                frame.expired = true;
                self.counts.incomplete += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(id: u64, index: usize, count: usize) -> Vec<u8> {
        let start = index * 1440;
        let len = (count - start).min(1440);
        let mut p = vec![7; 10 + len];
        p[0] = if start + len == count { 0x41 } else { 0x40 };
        p[1] = (id % 15 + 1) as u8;
        p[2] = 11;
        p[3] = 1;
        p[4..8].copy_from_slice(&(start as u32).to_be_bytes());
        p[8..10].copy_from_slice(&(len as u16).to_be_bytes());
        let marker = len.min(8);
        p[10..10 + marker].copy_from_slice(&id.to_be_bytes()[8 - marker..]);
        p
    }
    #[test]
    fn complete_requires_every_identified_chunk() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::Ddp, vec![7; 2883], 1, 32, now).unwrap();
        o.feed(&packet(1, 2, 2883), now);
        o.feed(&packet(1, 0, 2883), now);
        assert_eq!(o.counts.complete, 0);
        o.feed(&packet(1, 0, 2883), now);
        assert_eq!(o.counts.duplicates, 1);
        o.feed(&packet(1, 1, 2883), now);
        assert_eq!(o.counts.complete, 1);
        o.feed(&packet(2, 0, 2883), now);
        o.expire(now, true);
        assert_eq!(o.counts.incomplete, 1);
        assert!(o.counts.reordered > 0);
    }
    #[test]
    fn sequence_wrap_cannot_mix_delayed_frames() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::Ddp, vec![7; 2880], 1, 32, now).unwrap();
        o.feed(&packet(1, 0, 2880), now);
        o.feed(&packet(16, 1, 2880), now);
        assert_eq!(o.counts.complete, 0);
        o.feed(&packet(16, 0, 2880), now);
        assert_eq!(o.counts.complete, 1);
        o.feed(&packet(40, 0, 2880), now);
        o.feed(&packet(1, 1, 2880), now);
        assert_eq!(o.counts.late, 1);
        assert_eq!(o.counts.complete, 1);
    }
    #[test]
    fn malformed_and_expired_data_never_complete() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::Ddp, vec![7; 2880], 1, 32, now).unwrap();
        let mut p = packet(1, 0, 2880);
        p[1] = 4;
        o.feed(&p, now);
        p = packet(1, 0, 2880);
        p[19] = 99;
        o.feed(&p, now);
        p = packet(1, 0, 2880);
        p[0] = 0x41;
        o.feed(&p, now);
        assert_eq!(o.counts.invalid, 3);
        assert_eq!(o.counts.wrong_sequence, 1);
        o.feed(&packet(1, 0, 2880), now);
        o.expire(now + Duration::from_secs(1), false);
        o.feed(&packet(1, 1, 2880), now + Duration::from_secs(1));
        assert_eq!(o.counts.complete, 0);
        assert_eq!(o.counts.incomplete, 1);
        assert_eq!(o.counts.late, 1);
    }
    fn e131(id: u64, index: usize, count: usize) -> Vec<u8> {
        let mut p = vec![0; 638];
        p[..16].copy_from_slice(&[0, 16, 0, 0, 65, 83, 67, 45, 69, 49, 46, 49, 55, 0, 0, 0]);
        p[16..18].copy_from_slice(&0x726eu16.to_be_bytes());
        p[18..22].copy_from_slice(&4u32.to_be_bytes());
        p[38..40].copy_from_slice(&0x7258u16.to_be_bytes());
        p[40..44].copy_from_slice(&2u32.to_be_bytes());
        p[108] = 100;
        p[109..111].copy_from_slice(&63999u16.to_be_bytes());
        p[111] = ((id - 1) % 256) as u8;
        p[113..115].copy_from_slice(&((index + 1) as u16).to_be_bytes());
        p[115..126].copy_from_slice(&[0x72, 0x0b, 2, 0xa1, 0, 0, 0, 1, 2, 1, 0]);
        let used = (count - index * 510).min(510);
        p[126..126 + used].fill(7);
        let marker = used.min(8);
        p[126..126 + marker].copy_from_slice(&id.to_be_bytes()[8 - marker..]);
        p
    }
    #[test]
    fn e131_identity_does_not_depend_on_wrapped_sequence_or_sync() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::E131, vec![7; 513], 0, 512, now).unwrap();
        o.feed(&e131(1, 0, 513), now);
        o.feed(&e131(257, 1, 513), now);
        assert_eq!(o.counts.complete, 0);
        o.feed(&e131(257, 0, 513), now);
        assert_eq!(o.counts.complete, 1);
        let mut bad = e131(258, 0, 513);
        bad[637] = 1;
        o.feed(&bad, now);
        bad = e131(258, 0, 513);
        bad[125] = 1;
        o.feed(&bad, now);
        let mut sync = e131(1, 0, 513);
        sync.truncate(49);
        sync[16..18].copy_from_slice(&0x7021u16.to_be_bytes());
        sync[18..22].copy_from_slice(&8u32.to_be_bytes());
        sync[38..40].copy_from_slice(&0x700bu16.to_be_bytes());
        sync[40..44].copy_from_slice(&1u32.to_be_bytes());
        sync[45..47].copy_from_slice(&63999u16.to_be_bytes());
        o.feed(&sync, now);
        assert_eq!(o.counts.sync, 1);
        assert_eq!(o.counts.complete, 1);
        assert_eq!(o.counts.invalid, 2);
        o.expire(now, true);
        assert_eq!(o.counts.incomplete, 1);
    }
    #[test]
    fn e131_sources_cannot_join_frames_or_controls() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::E131, vec![7; 513], 0, 32, now).unwrap();
        let mut first = e131(1, 0, 513);
        first[22..38].fill(1);
        let mut malformed = first.clone();
        malformed[22..38].fill(9);
        malformed[134] = 99;
        o.feed(&malformed, now);
        assert_eq!(o.source_cid, None);
        o.feed(&first, now);
        let mut second = e131(1, 1, 513);
        second[22..38].fill(2);
        o.feed(&second, now);
        assert_eq!(o.counts.complete, 0);
        second[22..38].fill(1);
        o.feed(&second, now);
        assert_eq!(o.counts.complete, 1);
        let mut sync = first[..49].to_vec();
        sync[16..18].copy_from_slice(&0x7021u16.to_be_bytes());
        sync[18..22].copy_from_slice(&8u32.to_be_bytes());
        sync[38..40].copy_from_slice(&0x700bu16.to_be_bytes());
        sync[40..44].copy_from_slice(&1u32.to_be_bytes());
        sync[45..47].copy_from_slice(&63999u16.to_be_bytes());
        let mut discovery = vec![0; 124];
        discovery[..44].copy_from_slice(&sync[..44]);
        discovery[16..18].copy_from_slice(&0x706cu16.to_be_bytes());
        discovery[38..40].copy_from_slice(&0x7056u16.to_be_bytes());
        discovery[40..44].copy_from_slice(&2u32.to_be_bytes());
        discovery[112..114].copy_from_slice(&0x700cu16.to_be_bytes());
        discovery[114..118].copy_from_slice(&1u32.to_be_bytes());
        discovery[120..122].copy_from_slice(&1u16.to_be_bytes());
        discovery[122..124].copy_from_slice(&2u16.to_be_bytes());
        let mut termination = first;
        termination[112] = 0x40;
        for mut control in [sync, discovery, termination] {
            control[22..38].fill(2);
            o.feed(&control, now);
            control[22..38].fill(1);
            o.feed(&control, now);
        }
        assert_eq!(o.counts.invalid, 5);
        assert_eq!(o.counts.sync, 1);
        assert_eq!(o.counts.maintenance, 2);
        assert_eq!(o.counts.complete, 1);
    }
    #[test]
    fn opc_length_channel_pattern_and_duplicate_are_independent() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::Opc, vec![7; 12], 9, 32, now).unwrap();
        let mut p = vec![9, 0, 0, 12];
        p.extend_from_slice(&1u64.to_be_bytes());
        p.extend_from_slice(&[7; 4]);
        o.feed(&p, now);
        o.feed(&p, now);
        assert_eq!(o.counts.complete, 1);
        assert_eq!(o.counts.duplicates, 1);
        for offset in [0, 1, 3, 15] {
            let mut bad = p.clone();
            bad[offset] += 1;
            o.feed(&bad, now);
        }
        o.feed(&p[..15], now);
        assert_eq!(o.counts.invalid, 5);
        assert_eq!(o.counts.complete, 1);
    }
    #[test]
    fn identity_capacity_is_bounded_by_shortest_chunk() {
        let now = Instant::now();
        assert!(Oracle::new(Protocol::Ddp, vec![0; 1441], 1, 32, now).is_err());
        assert_eq!(
            Oracle::new(Protocol::Ddp, vec![0; 1443], 1, 32, now)
                .unwrap()
                .max_identity,
            0xffffff
        );
    }
    #[test]
    fn malformed_controls_do_not_pin_cid_or_count_maintenance() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::E131, vec![7; 513], 0, 32, now).unwrap();
        for universe in [0, 3, 64000, 65535] {
            let mut p = e131(1, 0, 513);
            p[112] = 0x40;
            p[113..115].copy_from_slice(&(universe as u16).to_be_bytes());
            p[22..38].fill(9);
            o.feed(&p, now);
        }
        let mut p = vec![0; 124];
        p[..44].copy_from_slice(&e131(1, 0, 513)[..44]);
        p[16..18].copy_from_slice(&0x706cu16.to_be_bytes());
        p[18..22].copy_from_slice(&8u32.to_be_bytes());
        p[38..40].copy_from_slice(&0x7056u16.to_be_bytes());
        p[40..44].copy_from_slice(&2u32.to_be_bytes());
        p[112..114].copy_from_slice(&0x700cu16.to_be_bytes());
        p[114..118].copy_from_slice(&1u32.to_be_bytes());
        for universes in [[0u16, 2], [1, 1], [2, 1], [1, 64000], [1, 3]] {
            for (slot, value) in p[120..].chunks_exact_mut(2).zip(universes) {
                slot.copy_from_slice(&value.to_be_bytes())
            }
            o.feed(&p, now);
        }
        assert_eq!(o.counts.invalid, 9);
        assert_eq!(o.counts.maintenance, 0);
        assert_eq!(o.source_cid, None);
        let mut valid = e131(1, 0, 513);
        valid[22..38].fill(1);
        o.feed(&valid, now);
        assert_eq!(o.source_cid, Some([1; 16]));
    }
}
