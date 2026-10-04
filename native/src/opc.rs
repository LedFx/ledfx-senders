//! OPC is one UDP datagram with an exact RGB payload length.
use crate::buffer::{Banks, NumericPolicy};
pub fn banks(pixels: usize, channel: u8) -> Result<Banks, &'static str> {
    if pixels == 0 || pixels > (65507 - 4) / 3 {
        return Err("OPC frame exceeds UDP payload ceiling");
    }
    let count = pixels * 3;
    let mut packet = vec![0; 4 + count];
    packet[0] = channel;
    packet[2..4].copy_from_slice(&(count as u16).to_be_bytes());
    Banks::with_policy(
        vec![packet],
        vec![(0, 0, 0, count)],
        count,
        4,
        NumericPolicy::Clip,
    )
}
