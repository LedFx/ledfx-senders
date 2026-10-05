use super::*;
use std::collections::VecDeque;
use std::sync::{Mutex, mpsc};
use std::thread;

#[derive(Default)]
struct Observed {
    receives: usize,
    sends: usize,
    sent: Vec<Vec<u8>>,
    closed: bool,
}

struct FakeIo {
    incoming: VecDeque<Vec<u8>>,
    blocked: bool,
    short: bool,
    interrupted: bool,
    delay: Duration,
    observed: Arc<Mutex<Observed>>,
    started: Option<mpsc::Sender<()>>,
    receiving: Option<mpsc::Sender<()>>,
}

impl DatagramIo for FakeIo {
    fn receive(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
        self.observed.lock().unwrap().receives += 1;
        if let Some(receiving) = self.receiving.take() {
            receiving.send(()).unwrap();
        }
        if let Some(data) = self.incoming.pop_front() {
            thread::sleep(self.delay);
            target[..data.len()].copy_from_slice(&data);
            Ok(data.len())
        } else {
            Err(ErrorKind::WouldBlock.into())
        }
    }
    fn send(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.observed.lock().unwrap().sends += 1;
        if let Some(started) = self.started.take() {
            started.send(()).unwrap();
        }
        if self.interrupted {
            return Err(ErrorKind::Interrupted.into());
        }
        if self.blocked {
            return Err(ErrorKind::WouldBlock.into());
        }
        self.observed.lock().unwrap().sent.push(data.to_vec());
        Ok(if self.short {
            data.len() - 1
        } else {
            data.len()
        })
    }
    fn close(&mut self) {
        self.observed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed = true;
    }
}

fn fake() -> (FakeIo, Arc<Mutex<Observed>>) {
    let observed = Arc::new(Mutex::new(Observed::default()));
    (
        FakeIo {
            incoming: VecDeque::new(),
            blocked: false,
            short: false,
            interrupted: false,
            delay: Duration::ZERO,
            observed: observed.clone(),
            started: None,
            receiving: None,
        },
        observed,
    )
}

fn config() -> HueConfig {
    HueConfig::new(
        "127.0.0.1:2100".parse().unwrap(),
        b"hue-fixture".to_vec(),
        (0..16).collect(),
        Duration::from_secs(5),
        Duration::from_millis(200),
        Duration::from_millis(200),
    )
    .unwrap()
}

fn candidate(io: FakeIo) -> Client<FakeIo> {
    candidate_at(io, Instant::now())
}

fn candidate_at(io: FakeIo, start: Instant) -> Client<FakeIo> {
    let mut session = Session::new(&config(), "127.0.0.1:3100".parse().unwrap()).unwrap();
    session.start(start).unwrap();
    // Drain the real initial flight; individual tests control adapter output separately.
    while session.poll_transmit().is_some() {}
    Client {
        io: Some(io),
        session: Some(session),
        pending: VecDeque::new(),
        queued_bytes: 0,
        handshake_inputs: 0,
    }
}

fn assert_disposed(client: &Client<FakeIo>, observed: &Arc<Mutex<Observed>>) {
    assert!(
        client.session.is_none(),
        "endpoint retained on terminal exit"
    );
    assert!(client.io.is_none(), "transport retained on terminal exit");
    assert!(observed.lock().unwrap().closed, "transport was not closed");
    assert_eq!(client.queued_bytes, 0);
    assert!(client.pending.is_empty());
}

#[test]
fn queue_overflow_closes_session() {
    let (io, observed) = fake();
    let mut client = candidate(io);
    for _ in 0..64 {
        client.enqueue(vec![1]).unwrap();
    }
    assert!(matches!(
        client.enqueue(vec![2]),
        Err(HueError::Protocol(_))
    ));
    assert_disposed(&client, &observed);
}

#[test]
fn queue_byte_overflow_closes_session() {
    let (io, observed) = fake();
    let mut client = candidate(io);
    for _ in 0..4 {
        client.enqueue(vec![1; 65_536]).unwrap();
    }
    assert!(matches!(
        client.enqueue(vec![2]),
        Err(HueError::Protocol(_))
    ));
    assert_disposed(&client, &observed);
}

#[test]
fn dequeue_releases_byte_budget() {
    let (io, observed) = fake();
    let mut client = candidate(io);
    for _ in 0..4 {
        client.enqueue(vec![1; 65_536]).unwrap();
    }
    client
        .service(Instant::now(), &Cancellation::new())
        .unwrap();
    assert_eq!(observed.lock().unwrap().sent.len(), 4);
    assert_eq!(client.queued_bytes, 0);
    for _ in 0..4 {
        client.enqueue(vec![2; 65_536]).unwrap();
    }
    assert_eq!(client.queued_bytes, 262_144);
}

#[test]
fn service_never_waits() {
    let (mut io, observed) = fake();
    io.blocked = true;
    let mut client = candidate(io);
    client.enqueue(vec![7; 9]).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = client.service(Instant::now(), &Cancellation::new());
        done_tx.send((result, client)).unwrap();
    });
    let (result, client) = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("service waited for readiness");
    worker.join().unwrap();
    result.unwrap();
    assert_eq!(observed.lock().unwrap().sends, 1);
    assert_eq!(client.pending.front().unwrap(), &vec![7; 9]);
    assert_eq!(client.queued_bytes, 9);
}

#[test]
fn service_reads_at_most_32() {
    let (mut io, observed) = fake();
    io.incoming = vec![Vec::new(); 40].into();
    let mut client = candidate(io);
    client
        .service(Instant::now(), &Cancellation::new())
        .unwrap();
    assert_eq!(observed.lock().unwrap().receives, 32);
    assert_eq!(client.io.as_ref().unwrap().incoming.len(), 8);
    client
        .service(Instant::now(), &Cancellation::new())
        .unwrap();
    assert_eq!(observed.lock().unwrap().receives, 41);
}

#[test]
fn handshake_input_cap_closes_candidate() {
    let (mut io, observed) = fake();
    io.incoming = vec![Vec::new(); 4_097].into();
    let mut client = candidate(io);
    for _ in 0..128 {
        client
            .service(Instant::now(), &Cancellation::new())
            .unwrap();
    }
    assert_eq!(observed.lock().unwrap().receives, 4_096);
    assert!(matches!(
        client.service(Instant::now(), &Cancellation::new()),
        Err(HueError::Protocol(_))
    ));
    assert_eq!(observed.lock().unwrap().receives, 4_097);
    assert_disposed(&client, &observed);
}

#[test]
fn short_udp_write_is_fatal() {
    let (mut io, observed) = fake();
    io.short = true;
    let mut client = candidate(io);
    client.enqueue(vec![1, 2, 3]).unwrap();
    assert!(matches!(
        client.service(Instant::now(), &Cancellation::new()),
        Err(HueError::Io(_))
    ));
    assert_disposed(&client, &observed);
}

#[test]
fn parser_error_is_fatal() {
    let (mut io, observed) = fake();
    io.incoming.push_back(vec![22]);
    let mut client = candidate(io);
    assert!(matches!(
        client.service(Instant::now(), &Cancellation::new()),
        Err(HueError::Protocol(_))
    ));
    assert_disposed(&client, &observed);
}

#[test]
fn expired_send_disposes_before_io() {
    let (io, observed) = fake();
    let mut client = candidate(io);
    assert!(matches!(
        client.send(b"frame", Instant::now(), &Cancellation::new()),
        Err(HueError::Timeout)
    ));
    assert_eq!(observed.lock().unwrap().sends, 0);
    assert_disposed(&client, &observed);
}

#[test]
fn expired_connect_has_no_io() {
    let (io, observed) = fake();
    let result = Client::connect_with(
        config(),
        io,
        "127.0.0.1:3100".parse().unwrap(),
        Arc::new(Cancellation::new()),
        Instant::now(),
    );
    assert!(matches!(result, Err(HueError::Timeout)));
    let state = observed.lock().unwrap();
    assert_eq!(state.sends, 0);
    assert_eq!(state.receives, 0);
    assert!(state.closed);
}

#[test]
fn deadline_is_not_renewed_by_noise() {
    let (mut io, observed) = fake();
    io.incoming = vec![Vec::new(); 4_096].into();
    io.delay = Duration::from_millis(1);
    let (done_tx, done_rx) = mpsc::channel();
    let start = Instant::now();
    thread::spawn(move || {
        done_tx
            .send(Client::connect_with(
                config(),
                io,
                "127.0.0.1:3100".parse().unwrap(),
                Arc::new(Cancellation::new()),
                start + Duration::from_millis(30),
            ))
            .unwrap();
    });
    let result = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("noise renewed handshake deadline");
    assert!(matches!(result, Err(HueError::Timeout)));
    assert!(observed.lock().unwrap().closed);
    assert!(observed.lock().unwrap().receives < 4_096);
}

#[test]
fn close_wakes_injected_handshake_wait() {
    let (mut io, observed) = fake();
    let (started_tx, started_rx) = mpsc::channel();
    io.started = Some(started_tx);
    let cancel = Arc::new(Cancellation::new());
    let worker_cancel = cancel.clone();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(Client::connect_with(
                config(),
                io,
                "127.0.0.1:3100".parse().unwrap(),
                worker_cancel,
                Instant::now() + Duration::from_secs(5),
            ))
            .unwrap();
    });
    started_rx.recv_timeout(Duration::from_millis(500)).unwrap();
    cancel.cancel();
    let result = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("cancel did not wake handshake");
    assert!(matches!(result, Err(HueError::Closed)));
    assert!(observed.lock().unwrap().closed);
}

#[test]
fn close_always_disposes_even_when_blocked() {
    let (mut io, observed) = fake();
    io.blocked = true;
    let mut client = candidate(io);
    client.enqueue(vec![7]).unwrap();
    assert!(matches!(
        client.close(Instant::now() + Duration::from_millis(30)),
        Err(HueError::Timeout)
    ));
    assert_disposed(&client, &observed);
    assert!(client.close(Instant::now()).is_ok());
}

#[test]
fn service_bounds_interrupted_sends() {
    let (mut io, observed) = fake();
    io.interrupted = true;
    let mut client = candidate(io);
    client.enqueue(vec![7]).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = client.service(Instant::now(), &Cancellation::new());
        done_tx.send((result, client)).unwrap();
    });
    let (result, client) = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("service spun on Interrupted");
    worker.join().unwrap();
    result.unwrap();
    assert_eq!(observed.lock().unwrap().sends, 1);
    assert_eq!(client.queued_bytes, 1);
}

#[test]
fn cancelled_service_disposes_without_io() {
    let (io, observed) = fake();
    let mut client = candidate(io);
    let cancel = Cancellation::new();
    cancel.cancel();
    assert!(matches!(
        client.service(Instant::now(), &cancel),
        Err(HueError::Closed)
    ));
    assert_eq!(observed.lock().unwrap().receives, 0);
    assert_disposed(&client, &observed);
}

#[test]
fn wrong_peer_is_not_delivered() {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.connect(peer.local_addr().unwrap()).unwrap();
    socket.set_nonblocking(true).unwrap();
    let destination = socket.local_addr().unwrap();
    let mut io = SocketIo(Some(socket));
    stranger.send_to(b"unrelated", destination).unwrap();
    peer.send_to(b"expected", destination).unwrap();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut buffer = [0; 65_535];
    loop {
        match io.receive(&mut buffer) {
            Ok(size) => {
                assert_eq!(&buffer[..size], b"expected");
                break;
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    }
    assert_eq!(
        io.receive(&mut buffer).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    io.close();
    assert!(io.0.is_none());
}

#[test]
fn close_wakes_handshake_wait() {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut config = config();
    config.peer = peer.local_addr().unwrap();
    let cancel = Arc::new(Cancellation::new());
    let worker_cancel = cancel.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        done_tx
            .send(Client::connect(
                config,
                worker_cancel,
                Instant::now() + Duration::from_secs(5),
            ))
            .unwrap();
    });
    let mut buffer = [0; 65_535];
    peer.recv(&mut buffer).unwrap();
    cancel.cancel();
    assert!(matches!(
        done_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("silent-peer connect ignored cancellation"),
        Err(HueError::Closed)
    ));
    worker.join().unwrap();
}

#[test]
fn fatal_alert_disposes_session() {
    let (mut io, observed) = fake();
    // Literal DTLS 1.2 epoch-zero fatal handshake_failure alert, sequence zero.
    io.incoming
        .push_back(vec![21, 254, 253, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 40]);
    let mut client = candidate(io);
    assert!(matches!(
        client.service(Instant::now(), &Cancellation::new()),
        Err(HueError::Protocol(_))
    ));
    assert_disposed(&client, &observed);
}

#[test]
fn retained_output_is_sent_once_after_would_block() {
    let (mut io, observed) = fake();
    io.blocked = true;
    let mut client = candidate(io);
    client.enqueue(vec![3, 1, 4]).unwrap();
    client
        .service(Instant::now(), &Cancellation::new())
        .unwrap();
    client.io.as_mut().unwrap().blocked = false;
    client
        .service(Instant::now(), &Cancellation::new())
        .unwrap();
    client
        .service(Instant::now(), &Cancellation::new())
        .unwrap();
    assert_eq!(observed.lock().unwrap().sent, vec![vec![3, 1, 4]]);
    assert_eq!(observed.lock().unwrap().sends, 2);
    assert_eq!(client.queued_bytes, 0);
}

#[test]
fn cancelled_flush_disposes_retained_output() {
    let (mut io, observed) = fake();
    io.blocked = true;
    let mut client = candidate(io);
    client.enqueue(vec![7]).unwrap();
    let cancel = Cancellation::new();
    cancel.cancel();
    assert!(matches!(
        client.send(b"frame", Instant::now() + Duration::from_secs(1), &cancel),
        Err(HueError::Closed)
    ));
    assert_disposed(&client, &observed);
    assert_eq!(observed.lock().unwrap().sends, 0);
}

#[test]
fn continuous_noise_does_not_hide_cancellation() {
    let (mut io, observed) = fake();
    io.incoming = vec![Vec::new(); 4_096].into();
    io.delay = Duration::from_millis(1);
    let (receiving_tx, receiving_rx) = mpsc::channel();
    io.receiving = Some(receiving_tx);
    let cancel = Arc::new(Cancellation::new());
    let worker_cancel = cancel.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        done_tx
            .send(Client::connect_with(
                config(),
                io,
                "127.0.0.1:3100".parse().unwrap(),
                worker_cancel,
                Instant::now() + Duration::from_secs(5),
            ))
            .unwrap();
    });
    receiving_rx
        .recv_timeout(Duration::from_millis(500))
        .unwrap();
    cancel.cancel();
    assert!(matches!(
        done_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("continuous input hid cancellation"),
        Err(HueError::Closed)
    ));
    worker.join().unwrap();
    assert!(observed.lock().unwrap().closed);
    assert!(observed.lock().unwrap().receives < 4_096);
}

#[test]
fn blocked_flush_drives_due_timer_with_original_deadline() {
    let (mut io, _) = fake();
    io.blocked = true;
    let mut client = candidate_at(io, Instant::now() - Duration::from_secs(2));
    client.enqueue(vec![7]).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = client.flush(
            Instant::now() + Duration::from_millis(30),
            &Cancellation::new(),
        );
        done_tx.send((result, client)).unwrap();
    });
    let (result, client) = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("retransmit renewed blocked output deadline");
    assert!(matches!(result, Err(HueError::Timeout)));
    assert_eq!(
        client.pending.len(),
        2,
        "due handshake retransmission was not driven"
    );
    assert_eq!(client.pending.back().unwrap()[0], 22);
}
