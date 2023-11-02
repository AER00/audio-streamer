use std::io::Read;
use std::net::{Ipv4Addr, UdpSocket};
use std::process::{Command, Stdio};
use std::time::Instant;
use byteorder::{ByteOrder, LittleEndian};
use anyhow::anyhow;
use clap::Parser;
// use rand::prelude::*;

const DATA_SILENCE: u8 = 0;
const DATA_SOUND: u8 = 1;
// const DATA_CONFIG: u8 = 2;

/// Stream PC audio
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Sample rate
    #[arg(short, long, default_value_t = 44100)]
    rate: u32,

    /// Remote ALSA's buffer size
    #[arg(short, long, default_value_t = 1024)]
    buffer: u16,

    /// Server address
    #[arg(short, long)]
    address: String,

    /// Default sink
    #[arg(short, long)]
    sink: u16,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect(args.address)?;

    socket.send(format!("\x0232 {} {}", args.rate, args.buffer).as_ref())?;

    let mut buf = vec![0u8; 1025];

    let command = Command::new("pw-record")
        .arg("--format=s32")
        .arg(format!("--rate={}", args.rate))
        .arg(format!("--target={}", args.sink))
        .arg("-")
        .stdout(Stdio::piped())
        .spawn()?;

    let mut audio = command.stdout
        .ok_or(anyhow!("cannot take pw-record stdout"))?;

    let mut silence;
    let mut size;

    loop {
        let start = Instant::now();

        size = audio.read(&mut buf[1..])? + 1;
        buf[0] = DATA_SOUND;

        silence = true;
        for x in &buf[1..size] {
            if *x != 0u8 {
                silence = false;
                break;
            }
        }

        if silence {
            buf[0] = DATA_SILENCE;
            LittleEndian::write_u16(&mut buf[1..3], size as u16);
            socket.send(&buf[..3])?;
            continue;
        }

        socket.send(&buf[..size])?;
    }
}
