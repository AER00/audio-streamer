mod capture;

use clap::Parser;
use std::net::{Ipv4Addr, UdpSocket};

/// Stream PC audio
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Sample rate
    #[arg(short, long, default_value_t = 0)]
    rate: u32,

    /// Remote ALSA's buffer size
    #[arg(short, long, default_value_t = 2048)]
    buffer: u16,

    /// Server address
    #[arg(short, long)]
    address: String,
}

pub fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_nonblocking(true)?;
    socket.connect(&args.address)?;

    capture::stream(socket, &args)
}
