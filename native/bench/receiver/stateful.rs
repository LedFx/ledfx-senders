//! Literal OSC and WLED wire parsing, independent of production sender modules.
use super::{Cow, Data, Protocol};
pub(super) fn tags(protocol: Protocol, count: usize) -> Vec<u8> {
    let s = match protocol {
        Protocol::OscOne => ",[fff]".to_owned(),
        Protocol::OscThree => ",fff".to_owned(),
        Protocol::OscChannels => ",f".to_owned(),
        Protocol::OscAll => format!(",{}", "[fff]".repeat(count / 12)),
        _ => String::new(),
    };
    s.into_bytes()
}
fn string(p: &[u8], start: usize) -> Result<(&[u8], usize), ()> {
    let end = start
        + p.get(start..)
            .ok_or(())?
            .iter()
            .position(|&b| b == 0)
            .ok_or(())?;
    let next = (end + 4) & !3;
    if p.get(end..next).ok_or(())?.iter().any(|&b| b != 0) {
        return Err(());
    }
    Ok((&p[start..end], next))
}
pub(super) fn osc<'a>(
    protocol: Protocol,
    p: &'a [u8],
    count: usize,
    tags: &[u8],
) -> Result<Data<'a>, ()> {
    let (path, next) = string(p, 0)?;
    let (actual, start) = string(p, next)?;
    if actual != tags {
        return Err(());
    }
    let suffix = path.strip_prefix(b"/bench/").ok_or(())?;
    if suffix.is_empty()
        || suffix.len() > 1 && suffix[0] == b'0'
        || suffix.iter().any(|b| !b.is_ascii_digit())
    {
        return Err(());
    }
    let index = std::str::from_utf8(suffix)
        .map_err(|_| ())?
        .parse::<usize>()
        .map_err(|_| ())?;
    let width = protocol.chunk_size(count);
    if index >= count / width || p.len() != start + width {
        return Err(());
    }
    Ok(Data {
        payload: Cow::Borrowed(&p[start..]),
        index,
        sequence: None,
    })
}
pub(super) fn realtime<'a>(
    protocol: Protocol,
    p: &'a [u8],
    expected: &[u8],
    timeout: u8,
) -> Result<Data<'a>, ()> {
    let count = expected.len();
    let kind = protocol.realtime_kind(count);
    if kind == 0 {
        if p.len() != count {
            return Err(());
        }
        return Ok(Data {
            payload: Cow::Borrowed(p),
            index: 0,
            sequence: None,
        });
    }
    if p.len() < 2
        || p[1] != timeout
        || p[0] != kind && !(protocol == Protocol::Adaptive && count <= 765 && p[0] == 1)
    {
        return Err(());
    }
    let payload = match p[0] {
        1 => {
            if !(p.len() - 2).is_multiple_of(4) || p.len() < 6 {
                return Err(());
            }
            let mut values = expected.to_vec();
            let mut last = None;
            for pixel in p[2..].chunks_exact(4) {
                let index = pixel[0] as usize;
                if index >= count / 3 || last.is_some_and(|i| index <= i) {
                    return Err(());
                }
                if last.is_none() && index != 0 {
                    return Err(());
                }
                values[index * 3..index * 3 + 3].copy_from_slice(&pixel[1..]);
                last = Some(index);
            }
            Cow::Owned(values)
        }
        2 => {
            if p.len() != 2 + count {
                return Err(());
            }
            Cow::Borrowed(&p[2..])
        }
        3 => {
            if p.len() != 2 + count / 3 * 4 {
                return Err(());
            }
            let mut values = Vec::with_capacity(count);
            for pixel in p[2..].chunks_exact(4) {
                if pixel[3] != 0 {
                    return Err(());
                }
                values.extend_from_slice(&pixel[..3]);
            }
            Cow::Owned(values)
        }
        4 => {
            if p.len() < 4 {
                return Err(());
            }
            let start = u16::from_be_bytes(p[2..4].try_into().unwrap()) as usize * 3;
            if start >= count
                || !start.is_multiple_of(1467)
                || p.len() != 4 + (count - start).min(1467)
            {
                return Err(());
            }
            return Ok(Data {
                payload: Cow::Borrowed(&p[4..]),
                index: start / 1467,
                sequence: None,
            });
        }
        _ => return Err(()),
    };
    Ok(Data {
        payload,
        index: 0,
        sequence: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::Oracle;
    use std::time::Instant;
    fn string(p: &mut Vec<u8>, s: &[u8]) {
        p.extend_from_slice(s);
        p.resize((p.len() + 4) & !3, 0)
    }
    fn packet(protocol: Protocol, id: u64, index: usize, count: usize) -> Vec<u8> {
        let mut p = vec![];
        string(&mut p, format!("/bench/{index}").as_bytes());
        string(&mut p, &tags(protocol, count));
        p.extend_from_slice(&(id as f32).to_be_bytes());
        for _ in 1..protocol.chunk_size(count) / 4 {
            p.extend_from_slice(&0.5f32.to_be_bytes())
        }
        p
    }
    #[test]
    fn osc_literal_paths_tags_padding_values_and_identity() {
        let now = Instant::now();
        for protocol in [
            Protocol::OscOne,
            Protocol::OscThree,
            Protocol::OscChannels,
            Protocol::OscAll,
        ] {
            let fixture = 0.5f32.to_be_bytes().repeat(6);
            let mut o = Oracle::new(protocol, fixture, 0, 32, now).unwrap();
            let chunks = 24 / protocol.chunk_size(24);
            for i in (0..chunks).rev() {
                o.feed(&packet(protocol, 1, i, 24), now)
            }
            assert_eq!(o.counts.complete, 1);
            assert_eq!(o.max_identity, 1 << 24);
            o.feed(&packet(protocol, 1, 0, 24), now);
            assert_eq!(o.counts.duplicates, 1);
            for mutation in 0..4 {
                let mut p = packet(protocol, 2, 0, 24);
                match mutation {
                    0 => p[1] = b'X',
                    1 => p[9] = 1,
                    2 => p[12] = b'x',
                    _ => p.push(0),
                }
                o.feed(&p, now);
            }
            assert_eq!(o.counts.invalid, 4);
        }
    }
    #[test]
    fn scalar_osc_delayed_packets_cannot_form_a_frame() {
        let now = Instant::now();
        let mut o = Oracle::new(Protocol::OscChannels, vec![0; 12], 0, 32, now).unwrap();
        o.feed(&packet(Protocol::OscChannels, 1, 0, 12), now);
        o.feed(&packet(Protocol::OscChannels, 2, 1, 12), now);
        o.feed(&packet(Protocol::OscChannels, 2, 2, 12), now);
        assert_eq!(o.counts.complete, 0);
        o.feed(&packet(Protocol::OscChannels, 2, 0, 12), now);
        assert_eq!(o.counts.complete, 1);
        o.feed(&packet(Protocol::OscChannels, (1 << 24) + 2, 0, 12), now);
        assert_eq!(o.counts.invalid, 1);
    }
    #[test]
    fn realtime_decoders_validate_white_indices_timeout_and_full_chunks() {
        let now = Instant::now();
        for (protocol, p) in [
            (Protocol::Drgb, vec![2, 1, 0, 0, 1, 7, 7, 7]),
            (Protocol::Warls, vec![1, 1, 0, 0, 0, 1]),
            (Protocol::Drgbw, vec![3, 1, 0, 0, 1, 0, 7, 7, 7, 0]),
            (Protocol::Raw, vec![0, 0, 1, 7, 7, 7]),
            (Protocol::Adaptive, vec![1, 1, 0, 0, 0, 1]),
        ] {
            let mut o = Oracle::new(protocol, vec![7; 6], 1, 32, now).unwrap();
            o.feed(&p, now);
            assert_eq!(o.counts.complete, 1);
            let mut bad = p;
            bad.push(7);
            o.feed(&bad, now);
            assert_eq!(o.counts.invalid, 1);
        }
        let mut o = Oracle::new(Protocol::Dnrgb, vec![7; 1470], 1, 32, now).unwrap();
        let mut first = vec![4, 1, 0, 0, 0, 0, 1];
        first.extend_from_slice(&vec![7; 1464]);
        let last = vec![4, 1, 1, 233, 0, 0, 1];
        o.feed(&first, now);
        assert_eq!(o.counts.complete, 0);
        o.feed(&last, now);
        assert_eq!(o.counts.complete, 1);
        let mut invalid = last;
        invalid[3] = 234;
        o.feed(&invalid, now);
        assert_eq!(o.counts.invalid, 1);
    }
}
