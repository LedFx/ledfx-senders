//! Sequential DDP coverage for real effect data, without synthetic identity marks.
//! Sequence reuse or reordered traffic can be ambiguous: these are structurally
//! complete assemblies, never uniquely identified delivered application frames.

use super::{cpu_seconds, socket};
use std::{
    io::{self, BufRead, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::mpsc,
    time::{Duration, Instant},
};

const SCOPE: &str = "sequential byte coverage with consistent 4-bit sequence; not unique frame identity; sequence-wrap and reorder ambiguity remains";
#[derive(Default)]
struct Counts {
    packets: u64,
    bytes: u64,
    pushes: u64,
    complete: u64,
    incomplete: u64,
    invalid: u64,
    gaps: u64,
}
struct Assembly {
    expected: usize,
    pending: Option<(u8, usize)>,
    counts: Counts,
}
impl Assembly {
    fn new(pixels: usize) -> Result<Self, &'static str> {
        if !(1..=1_000_000).contains(&pixels) {
            return Err("invalid RGB pixel count");
        }
        Ok(Self {
            expected: pixels * 3,
            pending: None,
            counts: Counts::default(),
        })
    }
    fn feed(&mut self, packet: &[u8]) {
        self.counts.packets += 1;
        self.counts.bytes += packet.len() as u64;
        if packet.len() < 10 {
            self.invalid();
            return;
        }
        let offset = u32::from_be_bytes(packet[4..8].try_into().unwrap()) as usize;
        let length = u16::from_be_bytes(packet[8..10].try_into().unwrap()) as usize;
        if ![0x40, 0x41].contains(&packet[0])
            || !(1..=15).contains(&packet[1])
            || packet[2] != 11
            || packet[3] != 1
            || length == 0
            || length != packet.len() - 10
            || offset
                .checked_add(length)
                .is_none_or(|end| end > self.expected)
        {
            self.invalid();
            return;
        }
        if offset == 0 {
            if self.pending.is_some() {
                self.counts.incomplete += 1;
            }
            self.pending = Some((packet[1], 0));
        }
        if self.pending == Some((packet[1], offset)) {
            self.pending = Some((packet[1], offset + length));
        } else {
            self.counts.gaps += 1;
            if self.pending.take().is_some() {
                self.counts.incomplete += 1;
            }
        }
        if packet[0] & 1 != 0 {
            self.counts.pushes += 1;
            if self.pending.is_some_and(|(_, end)| end == self.expected) {
                self.counts.complete += 1;
            } else {
                self.counts.incomplete += 1;
            }
            self.pending = None;
        }
    }
    // Final stop turns the unfinished coverage into one incomplete event;
    // snapshots deliberately keep it pending across measurement boundaries.
    fn finish(&mut self) {
        if self.pending.take().is_some() {
            self.counts.incomplete += 1;
        }
    }
    fn invalid(&mut self) {
        self.counts.invalid += 1;
        if self.pending.take().is_some() {
            self.counts.incomplete += 1;
        }
    }
}

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != 3 {
        return Err(
            "usage: ledfx-receiver ddp-structural <pixels> <portable|batched> <loopback-address>"
                .into(),
        );
    }
    let mut assembly = Assembly::new(args[0].parse()?)?;
    if !["portable", "batched"].contains(&args[1].as_str()) {
        return Err("invalid receive backend".into());
    }
    let bind: Ipv4Addr = args[2].parse()?;
    if !bind.is_loopback() {
        return Err("structural receiver requires loopback".into());
    }
    let mut receiver = socket::Receiver::bind(args[1] == "batched", bind)?;
    let (sender, commands) = mpsc::channel();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines().take(128) {
            let Ok(line) = line else { break };
            if sender.send(line.clone()).is_err() || line == "stop" {
                return;
            }
        }
        let _ = sender.send("stop".to_owned());
    });
    println!(
        "{{\"port\":{},\"receive_buffer_bytes\":{},\"backend\":\"{}\",\"unique_identity\":false,\"scope\":\"{}\"}}",
        receiver.port()?,
        receiver.buffer_size()?,
        receiver.backend,
        SCOPE
    );
    io::stdout().flush()?;
    let mut peer: Option<SocketAddrV4> = None;
    let mut unexpected = 0u64;
    let mut previous = None;
    let mut maximum_gap = Duration::ZERO;
    let mut stop = None;
    let started = Instant::now();
    loop {
        while let Ok(command) = commands.try_recv() {
            match command.as_str() {
                "snapshot" => {
                    report(
                        &assembly,
                        &receiver,
                        unexpected,
                        maximum_gap,
                        started.elapsed(),
                    );
                    io::stdout().flush()?;
                    maximum_gap = Duration::ZERO;
                    previous = None;
                }
                "stop" => {
                    stop = Some(Instant::now());
                }
                _ => return Err("unknown receiver command".into()),
            }
        }
        if stop.is_some_and(|t: Instant| t.elapsed() >= Duration::from_millis(200)) {
            break;
        }
        let now = Instant::now();
        match receiver.receive(|packet, address| {
            if !address.ip().is_loopback() || peer.is_some_and(|p| p != address) {
                unexpected += 1;
                return;
            }
            peer = Some(address);
            assembly.feed(packet);
        }) {
            Ok(n) => {
                if n > 0 {
                    if let Some(last) = previous {
                        maximum_gap = maximum_gap.max(now.duration_since(last));
                    }
                    previous = Some(now);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => receiver.wait()?,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    assembly.finish();
    report(
        &assembly,
        &receiver,
        unexpected,
        maximum_gap,
        started.elapsed(),
    );
    Ok(())
}
fn report(
    assembly: &Assembly,
    receiver: &socket::Receiver,
    unexpected: u64,
    gap: Duration,
    elapsed: Duration,
) {
    let c = &assembly.counts;
    let cpu = cpu_seconds();
    let number = |x: Option<f64>| {
        x.map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_owned())
    };
    let drops = receiver
        .kernel_drops()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "null".to_owned());
    println!(
        "{{\"counts\":[{},{},{},{},{}],\"incomplete_assembly_events\":{},\"gap_events\":{},\"unexpected_source\":{},\"receive_calls\":{},\"kernel_socket_drops\":{},\"interval_max_successful_receive_gap_seconds\":{},\"elapsed_seconds\":{},\"cpu_user_seconds\":{},\"cpu_system_seconds\":{},\"unique_identity\":false,\"scope\":\"{}\"}}",
        c.packets,
        c.bytes,
        c.pushes,
        c.complete,
        c.invalid,
        c.incomplete,
        c.gaps,
        unexpected,
        receiver.calls,
        drops,
        gap.as_secs_f64(),
        elapsed.as_secs_f64(),
        number(cpu.map(|x| x.0)),
        number(cpu.map(|x| x.1)),
        SCOPE
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(sequence: u8, offset: u32, payload: &[u8], push: bool) -> Vec<u8> {
        let mut out = vec![0x40 | u8::from(push), sequence, 11, 1];
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn stop_counts_a_pending_assembly_once_as_an_event() {
        let mut receiver = Assembly::new(4).unwrap();
        receiver.feed(&packet(1, 0, b"abcdef", false));
        assert_eq!(receiver.counts.incomplete, 0);
        receiver.finish();
        assert_eq!(receiver.counts.incomplete, 1);
        assert_eq!(receiver.counts.complete, 0);
        assert!(receiver.pending.is_none());
        receiver.finish();
        assert_eq!(receiver.counts.incomplete, 1);
    }

    #[test]
    fn arbitrary_pixels_are_accepted_without_fixture_or_marker_rewrite() {
        let mut receiver = Assembly::new(4).unwrap();
        receiver.feed(&packet(15, 0, b"abcdef", false));
        receiver.feed(&packet(15, 6, b"ghijkl", true));
        receiver.feed(&packet(1, 0, &[255; 12], true));
        assert_eq!(receiver.counts.complete, 2);
        assert_eq!(receiver.counts.invalid, 0);
    }

    #[test]
    fn missing_reordered_duplicate_or_changed_sequence_is_incomplete() {
        for packets in [
            vec![packet(1, 0, b"abc", false), packet(1, 9, b"jkl", true)],
            vec![
                packet(1, 6, b"ghi", false),
                packet(1, 0, b"abc", false),
                packet(1, 9, b"jkl", true),
            ],
            vec![
                packet(1, 0, b"abcdef", false),
                packet(1, 0, b"abcdef", false),
                packet(2, 6, b"ghijkl", true),
            ],
        ] {
            let mut receiver = Assembly::new(4).unwrap();
            for p in packets {
                receiver.feed(&p);
            }
            assert_eq!(receiver.counts.complete, 0);
            assert!(receiver.counts.incomplete > 0);
        }
    }

    #[test]
    fn repeated_sequence_cannot_prove_unique_application_frame_identity() {
        let mut receiver = Assembly::new(4).unwrap();
        // A delayed old tail with the same sequence is indistinguishable from
        // current payload. Count coverage, explicitly not unique identities.
        receiver.feed(&packet(1, 0, b"newnew", false));
        receiver.feed(&packet(1, 6, b"oldold", true));
        assert_eq!(receiver.counts.complete, 1);
    }

    #[test]
    fn malformed_lengths_flags_offsets_and_empty_payload_are_rejected() {
        let mut receiver = Assembly::new(4).unwrap();
        for p in [
            vec![0; 9],
            packet(1, 0, b"", true),
            packet(1, 11, b"abcd", true),
            packet(0, 0, b"abcdefghijkl", true),
            vec![0xff; 20],
        ] {
            receiver.feed(&p);
        }
        assert_eq!(receiver.counts.invalid, 5);
        assert_eq!(receiver.counts.complete, 0);
    }
}
