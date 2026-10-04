use super::*;
#[test]
fn logical_stride_does_not_include_wire_padding() {
    let mut l = Layout::new(1, 32765, 1, true, 1, 0, &[], &[], "RGB", "None").unwrap();
    l.pack(&[11, 22, 33]);
    assert_eq!(l.packets.len(), 3);
    for (i, p) in l.packets.iter().enumerate() {
        assert_eq!(&p[18..], &[(i as u8 + 1) * 11, 0]);
        assert_eq!(u16::from_le_bytes([p[14], p[15]]), 32765 + i as u16);
    }
    assert!(Layout::new(1, 32766, 1, true, 1, 0, &[], &[], "RGB", "None").is_err());
}

struct Probe {
    prefix: usize,
    fail: bool,
    closed: bool,
    deadlines: Vec<Instant>,
    packets: Vec<Vec<u8>>,
}
impl DatagramTransport for Probe {
    fn send_batch(&mut self, packets: &[Datagram<'_>], deadline: Instant) -> io::Result<usize> {
        self.deadlines.push(deadline);
        if self.fail {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "injected"));
        }
        let n = packets.len().min(self.prefix);
        self.packets
            .extend(packets[..n].iter().map(|p| p.bytes.to_vec()));
        Ok(n)
    }
    fn close(&mut self) {
        self.closed = true;
    }
}
fn state<T: DatagramTransport>(pixels: usize, transport: T) -> State<T> {
    let layout = Layout::new(pixels, 0, 510, true, 1, 0, &[], &[], "RGB", "Accurate").unwrap();
    let n = layout.output_count;
    State {
        committed: layout.packets.clone(),
        layout,
        input: Original {
            bytes: vec![31; pixels * 3],
            ..Original::default()
        },
        bytes: vec![0; n],
        floats: vec![0.0; n],
        doubles: vec![0.0; n],
        transport,
        destination: "127.0.0.1:6454".parse().unwrap(),
        sequence: 0,
        closed: false,
        datagrams: 0,
        wire_bytes: 0,
        errors: 0,
        cleanup_error: None,
    }
}
#[test]
fn accepted_prefix_sequence_atomic_commit_and_blackout_failure() {
    let mut s = state(
        400,
        Probe {
            prefix: 2,
            fail: false,
            closed: false,
            deadlines: Vec::new(),
            packets: Vec::new(),
        },
    );
    assert!(s.send(false).is_err());
    assert_eq!((s.sequence, s.datagrams), (2, 2));
    assert_eq!(s.committed[0][21], 0);
    s.transport.fail = true;
    assert!(s.send(false).is_err());
    assert_eq!(s.sequence, 2);
    s.transport.fail = false;
    s.transport.prefix = 1024;
    s.send(false).unwrap();
    assert_eq!(s.committed[0][12], 2);
    assert_eq!(s.committed[0][21], 31);
    s.transport.fail = true;
    s.close(true);
    assert!(s.closed && s.transport.closed && s.cleanup_error.is_some());
    let calls = s.transport.deadlines.len();
    s.close(true);
    assert_eq!(s.transport.deadlines.len(), calls);
}
#[test]
fn frame_and_cleanup_each_use_one_total_budget() {
    let mut s = state(
        140_000,
        Probe {
            prefix: 1024,
            fail: false,
            closed: false,
            deadlines: Vec::new(),
            packets: Vec::new(),
        },
    );
    s.send(false).unwrap();
    assert_eq!(s.transport.deadlines.len(), 2);
    assert_eq!(s.transport.deadlines[0], s.transport.deadlines[1]);
    s.close(true);
    assert_eq!(s.transport.deadlines.len(), 4);
    assert_eq!(s.transport.deadlines[2], s.transport.deadlines[3]);
    assert!(s.transport.deadlines[2] > s.transport.deadlines[0]);
    assert!(
        s.transport.packets[s.layout.packets.len()..]
            .iter()
            .all(|p| p[18..].iter().all(|b| *b == 0))
    );
}
#[test]
fn warm_native_transform_and_pack_allocate_nothing() {
    let mut s = state(
        50_000,
        MemoryTransport {
            capture: false,
            packets: Vec::new(),
        },
    );
    s.input.kind = 2;
    s.input.doubles = vec![3.5; 150_000];
    s.send(false).unwrap();
    let before = crate::buffer::test_allocation_count();
    for _ in 0..10 {
        s.send(false).unwrap();
    }
    assert_eq!(crate::buffer::test_allocation_count(), before);
}
#[test]
#[ignore]
fn benchmark_artnet_stages() {
    use std::hint::black_box;
    for pixels in [170, 50_000] {
        for kind in [0, 1, 2] {
            let mut s = state(
                pixels,
                MemoryTransport {
                    capture: false,
                    packets: Vec::new(),
                },
            );
            s.input.kind = kind;
            s.input.floats = vec![37.75; pixels * 3];
            s.input.doubles = vec![37.75; pixels * 3];
            for grouped in [false, true] {
                if grouped {
                    s.layout = Layout::new(
                        pixels,
                        0,
                        512,
                        true,
                        9,
                        7,
                        &[255, 128],
                        &[64],
                        "BRG",
                        "Accurate",
                    )
                    .unwrap();
                    s.committed = s.layout.packets.clone();
                }
                let n = if pixels == 170 { 20000 } else { 200 };
                for trial in 0..5 {
                    let start = Instant::now();
                    for _ in 0..n {
                        numeric(
                            black_box(&s.input),
                            &s.layout,
                            black_box(&mut s.bytes),
                            &mut s.floats,
                            &mut s.doubles,
                        )
                        .unwrap();
                    }
                    let numeric_ns = start.elapsed().as_nanos() / n;
                    let start = Instant::now();
                    for _ in 0..n {
                        s.layout.pack(black_box(&s.bytes));
                    }
                    let pack_ns = start.elapsed().as_nanos() / n;
                    let start = Instant::now();
                    for _ in 0..n {
                        s.send(false).unwrap();
                    }
                    println!(
                        "artnet-stage grouped={grouped} pixels={pixels} kind={kind} trial={trial} numeric_ns={numeric_ns} pack_ns={pack_ns} discard_ns={}",
                        start.elapsed().as_nanos() / n
                    );
                }
            }
        }
    }
}
