#![allow(dead_code)]
#[path = "../../src/hue/config.rs"]
mod config;
#[path = "../../src/hue/io.rs"]
mod io;
#[path = "../../src/hue/session.rs"]
mod session;

use config::{Cancellation, HueConfig, HueError};
use io::Client;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn unhex(input: &str) -> Result<Vec<u8>, HueError> {
    if input.len() % 2 != 0 {
        return Err(HueError::Configuration);
    }
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or(HueError::Configuration)?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or(HueError::Configuration)?;
            Ok((high * 16 + low) as u8)
        })
        .collect()
}

fn run() -> Result<(), HueError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 6 {
        return Err(HueError::Configuration);
    }
    let ip: IpAddr = args[0].parse().map_err(|_| HueError::Configuration)?;
    let port = args[1].parse().map_err(|_| HueError::Configuration)?;
    let identity = unhex(&args[2])?;
    let key = unhex(&args[3])?;
    let payload = unhex(&args[4])?;
    let budget = Duration::from_millis(args[5].parse().map_err(|_| HueError::Configuration)?);
    let config = HueConfig::new(
        SocketAddr::new(ip, port),
        identity,
        key,
        budget,
        budget,
        budget,
    )?;
    let cancel = Arc::new(Cancellation::new());
    let deadline = Instant::now()
        .checked_add(budget)
        .ok_or(HueError::Configuration)?;
    let mut client = Client::connect(config, cancel.clone(), deadline)?;
    client.send(&payload, deadline, &cancel)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
