use super::*;
use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Duration;
#[test]
fn accepted_prefixes_and_no_duplicate_retry() {
    for prefix in 0..=4 {
        let mut steps = VecDeque::from([Ok(prefix), Err(io::Error::other("failed"))]);
        let mut offsets = vec![];
        let now = Instant::now();
        let result = drive_batch(
            4,
            64,
            now + Duration::from_secs(1),
            || now,
            |offset, _| {
                offsets.push(offset);
                steps.pop_front().unwrap()
            },
            |_| Ok(()),
        );
        if prefix == 0 {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), prefix);
        }
        assert_eq!(
            offsets,
            if prefix == 0 || prefix == 4 {
                vec![0]
            } else {
                vec![0, prefix]
            }
        );
    }
}
#[test]
fn interrupted_would_block_and_readiness_deadline() {
    let now = Instant::now();
    let time = Cell::new(now);
    let waits = Cell::new(0);
    let mut steps = VecDeque::from([
        Err(io::Error::from(io::ErrorKind::Interrupted)),
        Err(io::Error::from(io::ErrorKind::WouldBlock)),
        Ok(1),
    ]);
    assert_eq!(
        drive_batch(
            1,
            64,
            now + Duration::from_secs(1),
            || time.get(),
            |_, _| steps.pop_front().unwrap(),
            |deadline| {
                waits.set(waits.get() + 1);
                assert_eq!(deadline, now + Duration::from_secs(1));
                Ok(())
            }
        )
        .unwrap(),
        1
    );
    assert_eq!(waits.get(), 1);
    let result = drive_batch(
        1,
        64,
        now + Duration::from_secs(1),
        || time.get(),
        |_, _| Err(io::Error::from(io::ErrorKind::WouldBlock)),
        |_| {
            time.set(now + Duration::from_secs(1));
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
}
#[test]
fn impossible_short_udp_write_is_rejected() {
    assert_eq!(
        complete_datagram(2, 3).unwrap_err().kind(),
        io::ErrorKind::WriteZero
    );
    assert_eq!(complete_datagram(3, 3).unwrap(), 1);
}

#[test]
fn real_socket_backends_preserve_batch_payloads() {
    for batched in [false, true] {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        // This test drains only after the full burst. Account for kernel skb
        // overhead as well as payload bytes, independent of host defaults.
        socket2::SockRef::from(&receiver)
            .set_recv_buffer_size(1024 * 1024)
            .unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let destination = match receiver.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        let payloads: Vec<_> = (0..130u8).map(|i| vec![i; 20]).collect();
        let packets: Vec<_> = payloads
            .iter()
            .map(|p| Datagram {
                bytes: p,
                destination,
            })
            .collect();
        let mut transport = SocketTransport::new(batched, 64).unwrap();
        let allocations = crate::buffer::test_allocation_count();
        assert_eq!(
            transport
                .send_batch(&packets, Instant::now() + Duration::from_secs(1))
                .unwrap(),
            130
        );
        assert_eq!(crate::buffer::test_allocation_count(), allocations);
        assert_eq!(
            transport.syscalls.get(),
            if batched && cfg!(target_os = "linux") {
                3
            } else {
                130
            }
        );
        assert_eq!(transport.readiness_waits.get(), 0);
        for i in 0..130u8 {
            let mut bytes = [0; 32];
            let n = receiver.recv(&mut bytes).unwrap();
            assert_eq!(&bytes[..n], &[i; 20]);
        }
        transport.close();
        assert!(
            transport
                .send_batch(&packets, Instant::now() + Duration::from_secs(1))
                .is_err()
        );
    }
}

#[test]
fn interrupted_readiness_retries_until_success_or_absolute_deadline() {
    let start = Instant::now();
    let ready = Cell::new(false);
    let waits = Cell::new(0);
    let accepted = drive_batch(
        1,
        64,
        start + Duration::from_secs(1),
        || start,
        |_, _| {
            if ready.get() {
                Ok(1)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        },
        |deadline| {
            assert_eq!(deadline, start + Duration::from_secs(1));
            waits.set(waits.get() + 1);
            if waits.get() == 1 {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                ready.set(true);
                Ok(())
            }
        },
    )
    .unwrap();
    assert_eq!(accepted, 1);
    assert_eq!(waits.get(), 2);
    for prefix in [0, 1] {
        let time = Cell::new(start);
        let waits = Cell::new(0);
        let mut progress = false;
        let result = drive_batch(
            2,
            64,
            start + Duration::from_secs(1),
            || time.get(),
            |offset, _| {
                assert_eq!(offset, if progress { prefix } else { 0 });
                if prefix > 0 && !progress {
                    progress = true;
                    Ok(prefix)
                } else {
                    Err(io::ErrorKind::WouldBlock.into())
                }
            },
            |deadline| {
                assert_eq!(deadline, start + Duration::from_secs(1));
                waits.set(waits.get() + 1);
                time.set(time.get() + Duration::from_millis(400));
                Err(io::ErrorKind::Interrupted.into())
            },
        );
        assert_eq!(waits.get(), 3);
        if prefix == 0 {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        } else {
            assert_eq!(result.unwrap(), prefix);
        }
    }
}
