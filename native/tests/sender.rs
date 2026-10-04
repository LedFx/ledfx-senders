use super::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
struct Script {
    steps: VecDeque<io::Result<usize>>,
    packets: Vec<Vec<u8>>,
    closed: bool,
    time: Option<Arc<Mutex<Instant>>>,
}
impl DatagramTransport for Script {
    fn send_batch(&mut self, p: &[Datagram<'_>], _: Instant) -> io::Result<usize> {
        if let Some(t) = &self.time {
            *t.lock().unwrap() += Duration::from_millis(600);
        }
        let n = self.steps.pop_front().unwrap_or(Ok(p.len()))?;
        self.packets
            .extend(p.iter().take(n).map(|p| p.bytes.to_vec()));
        Ok(n)
    }
    fn close(&mut self) {
        self.closed = true;
    }
}
fn sender() -> Sender<Script> {
    let banks = Banks::new(
        vec![vec![0; 638]; 3],
        vec![(0, 0, 0, 1), (1, 1, 0, 1), (2, 2, 0, 1)],
        3,
    )
    .unwrap();
    Sender::new(
        banks,
        Script {
            steps: VecDeque::new(),
            packets: vec![],
            closed: false,
            time: None,
        },
        vec!["127.0.0.1:5568".parse().unwrap(); 3],
        vec![0; 49],
        vec![vec![0; 120]],
        None,
    )
    .unwrap()
}
#[test]
fn prefixes_failure_preserves_commit_and_replays_committed() {
    for prefix in 0..=3 {
        let mut s = sender();
        s.banks.test_bytes(&[1, 2, 3]);
        s.send_prepared(0, 0.0).unwrap();
        s.service(0.0).unwrap();
        let before = s.banks.committed.clone();
        let counters = s.datagrams;
        s.transport.steps = if prefix == 0 {
            VecDeque::from([Err(io::Error::other("failed"))])
        } else {
            VecDeque::from([Ok(prefix), Err(io::Error::other("failed"))])
        };
        s.banks.test_bytes(&[9, 8, 7]);
        assert!(s.send_prepared(0, 0.1).is_err());
        assert_eq!(s.banks.committed, before);
        assert_eq!(s.datagrams, counters + prefix as u64);
        s.transport.steps.clear();
        let start = s.transport.packets.len();
        s.service(1.0).unwrap();
        assert_eq!(s.transport.packets.len() - start, 4);
        for (i, p) in s.transport.packets[start..start + 3].iter().enumerate() {
            assert_eq!(p[111], if i < prefix { 2 } else { 1 });
            assert_eq!(p[126..], before[i][126..]);
        }
    }
}
#[test]
fn partial_first_frame_requires_only_used_termination() {
    let mut s = sender();
    s.transport.steps = VecDeque::from([Ok(1), Err(io::Error::other("fail"))]);
    assert!(s.send_prepared(0, 0.0).is_err());
    let start = s.transport.packets.len();
    s.close(false, 0.0);
    assert_eq!(s.transport.packets.len() - start, 3);
    assert!(s.transport.packets[start..].iter().all(|p| p[112] == 0x40));
    assert!(s.transport.closed);
    s.close(true, 0.0);
    assert_eq!(s.transport.packets.len() - start, 3);
    assert!(s.send_prepared(0, 0.0).is_err());
    s.service(100.0).unwrap();
}
#[test]
fn deadline_stops_shutdown_and_releases_transport() {
    let mut s = sender();
    s.send_prepared(0, 0.0).unwrap();
    let time = Arc::new(Mutex::new(Instant::now()));
    let clock = time.clone();
    s.native_clock = Box::new(move || *clock.lock().unwrap());
    s.transport.time = Some(time);
    let start = s.datagrams;
    s.close(true, 0.0);
    assert!(s.datagrams - start <= 7);
    assert!(s.transport.closed);
}
#[test]
fn zero_progress_and_expired_deadline_are_errors() {
    let mut s = sender();
    s.transport.steps = VecDeque::from([Ok(0)]);
    assert!(s.send_prepared(0, 0.0).is_err());
    assert_eq!(s.datagrams, 0);
    s.send_prepared(0, 0.0).unwrap();
    let before = s.datagrams;
    let clock = Arc::new(Mutex::new(Instant::now()));
    let c = clock.clone();
    s.native_clock = Box::new(move || {
        let mut now = c.lock().unwrap();
        *now += Duration::from_secs(1);
        *now
    });
    assert!(s.service(0.0).is_err());
    assert_eq!(s.datagrams, before);
}

#[test]
fn discard_sender_does_not_allocate_after_setup() {
    let banks = Banks::new(vec![vec![0; 638]; 2049], vec![(0, 0, 0, 1)], 1).unwrap();
    let mut sender = Sender::new(
        banks,
        Transport::Memory(MemoryTransport {
            capture: false,
            packets: vec![],
        }),
        vec!["127.0.0.1:5568".parse().unwrap(); 2049],
        vec![0; 49],
        vec![vec![0; 120]],
        None,
    )
    .unwrap();
    let before = crate::buffer::test_allocation_count();
    for i in 0..100 {
        sender.send_prepared(0, i as f64).unwrap();
        sender.service(i as f64).unwrap();
    }
    assert_eq!(crate::buffer::test_allocation_count(), before);
}

#[test]
fn no_discovery_or_refresh_without_complete_first_frame() {
    for prefix in [1, 3] {
        let mut sender = sender();
        sender.service(100.0).unwrap();
        assert!(sender.transport.packets.is_empty());
        sender.transport.steps = VecDeque::from([Ok(prefix), Err(io::Error::other("failure"))]);
        assert!(sender.send_prepared(0, 0.0).is_err());
        assert!(!sender.committed);
        let before = sender.datagrams;
        sender.service(100.0).unwrap();
        assert_eq!(sender.datagrams, before);
        assert_eq!(sender.transport.packets.len(), prefix);
        sender.transport.steps.clear();
        let start = sender.transport.packets.len();
        sender.send_prepared(0, 100.0).unwrap();
        assert_eq!(
            sender.transport.packets[start..]
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![638, 638, 638, 49]
        );
        sender.service(100.0).unwrap();
        assert_eq!(sender.transport.packets.last().unwrap().len(), 120);
    }
}
