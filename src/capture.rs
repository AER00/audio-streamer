#[cfg(target_os = "linux")]
mod pipewire;
#[cfg(target_os = "linux")]
pub use pipewire::stream;

#[cfg(target_os = "windows")]
mod wasapi;
#[cfg(target_os = "windows")]
pub use wasapi::stream;

#[cfg(target_os = "macos")]
mod coreaudio;
#[cfg(target_os = "macos")]
pub use coreaudio::stream;

use byteorder::{ByteOrder, LittleEndian};
use std::net::UdpSocket;
use std::time::Duration;

/// Largest UDP payload we emit, so a datagram never needs IP fragmentation.
pub const MAX_PAYLOAD: usize = 1400;

/// Every packet starts with a tag byte followed by a u32 LE sequence number.
pub const HEADER_SIZE: usize = 5;

pub const PROTOCOL_VERSION: u8 = 2;

/// Resend the config this often so a restarted server re-syncs.
pub const CONFIG_INTERVAL: Duration = Duration::from_secs(2);

const DATA_SILENCE: u8 = 0;
const DATA_SOUND: u8 = 1;
const DATA_CONFIG: u8 = 2;

#[cfg(target_os = "linux")]
pub fn is_silence(bin: &[u8]) -> bool {
    for x in bin {
        if *x != 0 {
            return false;
        }
    }
    return true;
}

/// Payload size in bytes, aligned to a whole frame.
pub fn max_payload(sample_bytes: usize, channels: usize) -> usize {
    let frame = (sample_bytes * channels.max(1)).max(1);
    (MAX_PAYLOAD / frame).max(1) * frame
}

/// Write the tag and sequence number into the start of a packet buffer.
pub fn write_header(buf: &mut [u8], tag: u8, seq: u32) {
    buf[0] = tag;
    LittleEndian::write_u32(&mut buf[1..HEADER_SIZE], seq);
}

/// The config packet sent to the server before any audio.
pub fn config_packet(bits: usize, rate: u32, buffer: u16, channels: usize) -> String {
    format!(
        "{}{} {} {} {} {}",
        DATA_CONFIG as char, PROTOCOL_VERSION, bits, rate, buffer, channels
    )
}

/// Tell the server which format follows and how to configure playback.
pub fn send_config(
    sender: &UdpSocket,
    bits: usize,
    rate: u32,
    buffer: u16,
    channels: usize,
) -> anyhow::Result<()> {
    sender.send(config_packet(bits, rate, buffer, channels).as_bytes())?;
    Ok(())
}

/// Announce silence covering `byte_count` worth of samples.
pub fn send_silence(
    sender: &UdpSocket,
    buf: &mut Vec<u8>,
    seq: u32,
    byte_count: usize,
) -> anyhow::Result<()> {
    let size = HEADER_SIZE + 2;
    if buf.len() < size {
        buf.resize(size, 0);
    }
    write_header(buf, DATA_SILENCE, seq);
    LittleEndian::write_u16(&mut buf[HEADER_SIZE..size], byte_count as u16);
    sender.send(&buf[..size])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_stays_within_the_mtu_and_frame_aligned() {
        for (sample_bytes, channels) in [(2, 2), (4, 2), (2, 6), (4, 8)] {
            let payload = max_payload(sample_bytes, channels);
            assert!(payload <= MAX_PAYLOAD);
            assert_eq!(payload % (sample_bytes * channels), 0);
        }
    }

    #[test]
    fn payload_is_positive_even_for_absurd_channel_counts() {
        assert!(max_payload(4, 64) > 0);
    }

    #[test]
    fn header_round_trips() {
        let mut buf = [0u8; HEADER_SIZE];
        write_header(&mut buf, DATA_SOUND, 0xDEAD_BEEF);
        assert_eq!(buf[0], DATA_SOUND);
        assert_eq!(LittleEndian::read_u32(&buf[1..HEADER_SIZE]), 0xDEAD_BEEF);
    }

    #[test]
    fn config_packet_is_versioned() {
        assert_eq!(config_packet(16, 48_000, 2048, 2), "\u{2}2 16 48000 2048 2");
    }
}
