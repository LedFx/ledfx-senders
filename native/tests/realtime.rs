use super::*;
struct Probe {
    prefix: usize,
    deadlines: Vec<Instant>,
}
impl DatagramTransport for Probe {
    fn send_batch(&mut self, packets: &[Datagram<'_>], deadline: Instant) -> io::Result<usize> {
        self.deadlines.push(deadline);
        Ok(self.prefix.min(packets.len()))
    }
    fn close(&mut self) {}
}
fn state(pixels: usize, osc: bool) -> State<Probe> {
    let layout = if osc {
        Layout::Osc(
            osc::Layout::new(
                pixels * 3,
                "Three_Addresses",
                vec![b"/test".to_vec(); pixels * 3],
            )
            .unwrap(),
        )
    } else {
        Layout::realtime(pixels, "DNRGB", 1).unwrap()
    };
    State {
        current: Original {
            bytes: vec![42; pixels * 3],
            ..Original::default()
        },
        previous: Original::default(),
        initialized: false,
        bytes: vec![0; pixels * 3],
        floats: vec![0.0; pixels * 3],
        mask: vec![false; pixels],
        committed: layout.packets().to_vec(),
        layout,
        transport: Probe {
            prefix: 1024,
            deadlines: vec![],
        },
        destination: "127.0.0.1:1234".parse().unwrap(),
        minimise: true,
        interval: 0.5,
        last: None,
        closed: false,
        datagrams: 0,
        wire_bytes: 0,
        errors: 0,
        attempts: 0,
        suppressed: 0,
        frames: 0,
    }
}
#[test]
fn partial_frame_never_commits_suppression_or_refresh() {
    for osc in [false, true] {
        let mut s = state(1000, osc);
        let before = s.committed.clone();
        s.transport.prefix = 1;
        assert!(s.send(0.0).is_err());
        assert!(!s.initialized);
        assert_eq!(s.last, None);
        assert_eq!(s.committed, before);
        assert_eq!(s.datagrams, 1);
        s.transport.prefix = 1024;
        s.send(0.1).unwrap();
        assert!(s.initialized);
        assert_eq!(s.frames, 1);
        s.current.bytes = vec![42; 3000];
        s.send(0.2).unwrap();
        assert_eq!(s.suppressed, 1);
    }
}
#[test]
fn all_osc_packets_have_one_budget() {
    let mut s = state(1000, true);
    let now = Instant::now();
    s.send(0.0).unwrap();
    assert_eq!(s.transport.deadlines.len(), 3);
    assert!(
        s.transport
            .deadlines
            .iter()
            .all(|d| *d == s.transport.deadlines[0])
    );
    assert!(s.transport.deadlines[0] >= now + Duration::from_millis(200));
}
#[test]
fn invalid_late_value_preserves_wire_counters_and_time() {
    let mut s = state(1000, true);
    s.send(0.0).unwrap();
    s.current.kind = 2;
    s.current.doubles = vec![1.0; 3000];
    s.current.doubles[2999] = f64::NAN;
    let before = (
        s.committed.clone(),
        s.datagrams,
        s.errors,
        s.attempts,
        s.last,
    );
    assert_eq!(s.send(1.0).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        (s.committed, s.datagrams, s.errors, s.attempts, s.last),
        before
    );
}

#[test]
fn equal_typed_osc_frames_keep_validation_and_signed_zero_contracts() {
    for kind in 0..=4 {
        let mut s = state(1, true);
        fn input(s: &mut State<Probe>, kind: u8) {
            s.current.kind = kind;
            s.current.bytes = vec![0; 3];
            s.current.floats = vec![-0.0; 3];
            s.current.doubles = vec![-0.0; 3];
            s.current.signed = vec![0; 3];
            s.current.unsigned = vec![0; 3];
        }
        input(&mut s, kind);
        s.send(0.0).unwrap();
        let committed = s.committed.clone();
        input(&mut s, kind);
        s.current.floats.fill(0.0);
        s.current.doubles.fill(0.0);
        s.send(0.1).unwrap();
        assert_eq!(s.suppressed, 1);
        assert_eq!(s.committed, committed);
        if kind == 1 || kind == 2 || kind == 4 {
            s.current.floats[2] = f32::INFINITY;
            s.current.doubles[2] = f64::NAN;
            s.current.unsigned[2] = u64::MAX;
            assert_eq!(s.send(0.2).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert_eq!(s.attempts, 2);
            assert_eq!(s.committed, committed);
        }
        // A failed changed frame may dirty conversion scratch, but not identity.
        input(&mut s, kind);
        s.current.bytes[0] = 1;
        s.current.floats[0] = 1.0;
        s.current.doubles[0] = 1.0;
        s.current.signed[0] = 1;
        s.current.unsigned[0] = 1;
        s.transport.prefix = 0;
        assert!(s.send(0.3).is_err());
        if kind == 1 || kind == 2 || kind == 4 {
            s.current.floats[2] = f32::NAN;
            s.current.doubles[2] = f64::INFINITY;
            s.current.unsigned[2] = u64::MAX;
            let attempts = s.attempts;
            assert_eq!(
                s.send(0.35).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(s.attempts, attempts);
        }
        input(&mut s, kind);
        s.transport.prefix = 1024;
        s.send(0.4).unwrap();
        assert_eq!(s.suppressed, 2);
        assert_eq!(s.committed, committed);
    }
}

#[test]
fn malformed_equal_zero_rare_tokens_are_rejected_before_suppression() {
    let mut s = state(1, true);
    s.current.kind = 5;
    s.current.rare = vec![0; 9];
    s.send(0.0).unwrap();
    s.current.kind = 5;
    s.current.rare = vec![0; 9];
    s.current.rare[2] = 1 << 40; // Reserved bits cannot hide behind canonical zero.
    assert_eq!(s.send(0.1).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!((s.attempts, s.suppressed), (1, 0));
}

#[test]
fn realtime_keepalive_restores_successful_values_after_failed_changed_frame() {
    let mut s = state(1000, false);
    s.send(0.0).unwrap();
    let expected = s.committed.clone();
    s.current.bytes = vec![43; 3000];
    s.transport.prefix = 1;
    assert!(s.send(0.1).is_err());
    s.current.bytes = vec![42; 3000];
    s.transport.prefix = 1024;
    s.send(2.0).unwrap();
    assert_eq!(s.committed, expected);
    assert_eq!(s.frames, 2);
    assert_eq!(s.last, Some(2.0));
}
