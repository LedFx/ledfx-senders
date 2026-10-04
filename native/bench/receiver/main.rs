//! Loopback-only benchmark receiver. Production senders never import this binary.
mod oracle;
mod socket;
use oracle::{Oracle, Protocol};
use std::{
    io::{self, BufRead, Read, Write},
    net::SocketAddrV4,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
fn cpu_seconds() -> Option<(f64, f64)> {
    // SAFETY: getrusage initializes this valid writable structure on success.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    Some((
        usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1e6,
        usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1e6,
    ))
}
#[cfg(not(unix))]
fn cpu_seconds() -> Option<(f64, f64)> {
    None
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        return Err("usage: ledfx-receiver <ddp|e131|opc> <raw-RGB-fixture> <portable|batched> <window> <identifier>".into());
    }
    let protocol = Protocol::parse(&args[1])?;
    if !["portable", "batched"].contains(&args[3].as_str()) {
        return Err("invalid receive backend".into());
    }
    let mut fixture = Vec::new();
    std::fs::File::open(&args[2])?
        .take(3_000_001)
        .read_to_end(&mut fixture)?;
    let window: usize = args[4].parse()?;
    let mut oracle = Oracle::new(protocol, fixture, args[5].parse()?, window, Instant::now())?;
    let mut receiver = socket::Receiver::new(args[3] == "batched")?;
    let stopped = Arc::new(AtomicBool::new(false));
    let submitted = Arc::new(AtomicU64::new(0));
    let stop = Arc::clone(&stopped);
    let frames = Arc::clone(&submitted);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = io::BufReader::new(io::stdin().take(128)).read_line(&mut line);
        if result.is_ok()
            && let Some(count) = line
                .strip_prefix("stop ")
                .and_then(|s| s.trim().parse::<u64>().ok())
        {
            frames.store(count, Ordering::Release);
        }
        stop.store(true, Ordering::Release);
    });
    println!(
        "{{\"port\":{},\"receive_buffer_bytes\":{},\"backend\":\"{}\",\"identity_max\":{},\"reorder_window\":{}}}",
        receiver.port()?,
        receiver.buffer_size()?,
        receiver.backend,
        oracle.max_identity,
        window
    );
    io::stdout().flush()?;
    let start = Instant::now();
    let cpu = cpu_seconds();
    let mut peer: Option<SocketAddrV4> = None;
    let mut unexpected = 0u64;
    let mut expiry = Instant::now();
    let mut empty_since = None;
    let mut drain = None;
    let mut previous_receive = None;
    let mut max_receive_gap = Duration::ZERO;
    let mut first_packet = None;
    let mut last_packet = None;
    let mut packets_after_stop = 0usize;
    let mut empty_polls = 0u64;
    loop {
        let now = Instant::now();
        if stopped.load(Ordering::Acquire) && drain.is_none() {
            drain = Some(now);
        }
        if drain.is_some_and(|started| {
            now.duration_since(started) >= Duration::from_millis(200)
                || empty_since
                    .is_some_and(|empty| now.duration_since(empty) >= Duration::from_millis(20))
        }) {
            break;
        }
        if now.duration_since(expiry) >= Duration::from_millis(100) {
            oracle.expire(now, false);
            expiry = now;
        }
        match receiver.receive(|packet, address| {
            if !address.ip().is_loopback() || peer.is_some_and(|p| p != address) {
                unexpected += 1;
                return;
            }
            peer = Some(address);
            oracle.feed(packet, now);
        }) {
            Ok(n) => {
                if n > 0 {
                    first_packet.get_or_insert(now);
                    last_packet = Some(now);
                    if let Some(previous) = previous_receive {
                        max_receive_gap = max_receive_gap.max(now.duration_since(previous));
                    }
                    previous_receive = Some(now);
                    if drain.is_some() {
                        packets_after_stop += n;
                    }
                    empty_since = None;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                empty_polls += 1;
                if drain.is_some() && empty_since.is_none() {
                    empty_since = Some(now);
                }
                receiver.wait()?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    oracle.expire(Instant::now(), true);
    let elapsed = start.elapsed().as_secs_f64();
    let cpu = cpu
        .zip(cpu_seconds())
        .map(|((a, b), (c, d))| (c - a, d - b));
    let optional = |v: Option<f64>| {
        v.map(|n| n.to_string())
            .unwrap_or_else(|| "null".to_owned())
    };
    let cpu_user = optional(cpu.map(|(user, _)| user));
    let cpu_system = optional(cpu.map(|(_, system)| system));
    let cpu = optional(cpu.map(|(user, system)| user + system));
    let kernel_drops = receiver
        .kernel_drops()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let first_packet = optional(first_packet.map(|t| t.duration_since(start).as_secs_f64()));
    let last_packet = optional(last_packet.map(|t| t.duration_since(start).as_secs_f64()));
    let drain_seconds = optional(drain.map(|t| t.elapsed().as_secs_f64()));
    let c = &oracle.counts;
    let submitted = submitted.load(Ordering::Acquire);
    println!(
        "{{\"packets\":{},\"bytes\":{},\"complete_frames\":{},\"incomplete_frames\":{},\"highest_identity\":{},\"incomplete_identity_received_chunks_sample\":{},\"submitted_frames\":{},\"unseen_frames\":{},\"invalid_packets\":{},\"duplicate_packets\":{},\"late_packets\":{},\"reordered_packets\":{},\"sync_packets\":{},\"maintenance_packets\":{},\"wrong_sequence\":{},\"unexpected_source\":{},\"receive_calls\":{},\"kernel_socket_drops\":{},\"first_packet_seconds\":{},\"last_packet_seconds\":{},\"max_successful_receive_gap_seconds\":{},\"empty_polls\":{},\"packets_after_stop\":{},\"drain_seconds\":{},\"seconds\":{},\"cpu_seconds\":{},\"cpu_user_seconds\":{},\"cpu_system_seconds\":{},\"e131_scope\":\"identified complete data frames; sync packets validated separately, not falsely assigned across sequence wraps\"}}",
        c.datagrams,
        c.bytes,
        c.complete,
        c.incomplete,
        oracle.highest_identity(),
        oracle.incomplete_sample(),
        submitted,
        submitted.saturating_sub(c.complete + c.incomplete),
        c.invalid,
        c.duplicates,
        c.late,
        c.reordered,
        c.sync,
        c.maintenance,
        c.wrong_sequence,
        unexpected,
        receiver.calls,
        kernel_drops,
        first_packet,
        last_packet,
        max_receive_gap.as_secs_f64(),
        empty_polls,
        packets_after_stop,
        drain_seconds,
        elapsed,
        cpu,
        cpu_user,
        cpu_system
    );
    Ok(())
}
