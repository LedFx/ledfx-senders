use super::*;

struct Probe {
    prefix: usize,
    fail: bool,
    deadlines: Vec<Instant>,
}
impl DatagramTransport for Probe {
    fn send_batch(&mut self, packets: &[Datagram<'_>], deadline: Instant) -> io::Result<usize> {
        self.deadlines.push(deadline);
        if self.fail {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "injected"));
        }
        Ok(packets.len().min(self.prefix))
    }
    fn close(&mut self) {}
}
fn state(count: usize) -> State<Probe> {
    State {
        banks: banks(count, 1).unwrap(),
        transport: Probe {
            prefix: 1024,
            fail: false,
            deadlines: Vec::new(),
        },
        destination: "127.0.0.1:4048".parse().unwrap(),
        ddp: true,
        sequence: 1,
        closed: false,
        datagrams: 0,
        bytes: 0,
        errors: 0,
    }
}
#[test]
fn accepted_prefix_and_failure_do_not_commit() {
    let mut s = state(1440 * 3);
    s.banks.test_bytes(&vec![42; 1440 * 3]);
    s.transport.prefix = 2;
    assert_eq!(s.send(0).unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        (s.datagrams, s.bytes, s.errors, s.sequence),
        (2, 2900, 1, 2)
    );
    assert_eq!(s.banks.committed[0][10], 0);
    s.transport.fail = true;
    assert!(s.send(0).is_err());
    assert_eq!(
        (s.datagrams, s.bytes, s.errors, s.sequence),
        (2, 2900, 2, 3)
    );
    s.transport.fail = false;
    s.transport.prefix = 1024;
    s.send(0).unwrap();
    assert_eq!(s.banks.committed[0][10], 42);
    assert_eq!(s.banks.committed[0][1], 4);
}
#[test]
fn all_chunks_share_one_200ms_budget() {
    let mut s = state(1_500_000);
    let before = Instant::now();
    s.send(0).unwrap();
    assert_eq!(s.transport.deadlines.len(), 2);
    assert!(
        s.transport
            .deadlines
            .iter()
            .all(|d| *d == s.transport.deadlines[0])
    );
    assert!(s.transport.deadlines[0] >= before + Duration::from_millis(200));
    assert!(s.transport.deadlines[0] <= Instant::now() + Duration::from_millis(200));
    assert_eq!(s.datagrams, 1042);
}
#[test]
fn warmed_ddp_discard_allocates_nothing() {
    let mut s = State {
        banks: banks(150_000, 1).unwrap(),
        transport: MemoryTransport {
            capture: false,
            packets: Vec::new(),
        },
        destination: "127.0.0.1:4048".parse().unwrap(),
        ddp: true,
        sequence: 1,
        closed: false,
        datagrams: 0,
        bytes: 0,
        errors: 0,
    };
    s.send(0).unwrap();
    let before = crate::buffer::test_allocation_count();
    for _ in 0..10 {
        s.send(0).unwrap();
    }
    assert_eq!(crate::buffer::test_allocation_count(), before);
}
