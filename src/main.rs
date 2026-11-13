mod capture;
#[cfg(feature = "opus")]
mod codec;

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

    /// Sample bit depth (16 or 32). 0 follows the source format
    #[arg(long, default_value_t = 0)]
    bits: u16,

    /// Capture buffer in frames. 0 keeps the device default.
    /// Defaults to 256 on macOS to reduce latency.
    #[arg(long, default_value_t = if cfg!(target_os = "macos") { 256 } else { 0 })]
    frames: u32,

    /// Audio codec: pcm or opus (opus requires the `opus` cargo feature)
    #[arg(long, default_value = "pcm")]
    codec: String,

    /// Opus bitrate in bits per second (500..=512000)
    #[arg(long, default_value_t = 96_000)]
    bitrate: u32,

    /// Server address
    #[arg(short, long)]
    address: String,
}

impl Args {
    pub fn opus(&self) -> bool {
        self.codec.eq_ignore_ascii_case(capture::CODEC_OPUS)
    }
}

pub fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.codec.to_ascii_lowercase().as_str() {
        capture::CODEC_PCM | capture::CODEC_OPUS => {}
        other => anyhow::bail!("unknown codec: {}", other),
    }

    #[cfg(not(feature = "opus"))]
    if args.opus() {
        anyhow::bail!("this build has no opus support; rebuild with --features opus");
    }

    if args.opus() && !(500..=512_000).contains(&args.bitrate) {
        anyhow::bail!("opus bitrate must be between 500 and 512000");
    }

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_nonblocking(true)?;
    socket.connect(&args.address)?;

    capture::stream(socket, &args)
}
