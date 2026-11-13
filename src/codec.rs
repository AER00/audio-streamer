//! Optional Opus encoding, enabled with the `opus` cargo feature.
//!
//! Opus always runs at 48 kHz with 20 ms frames. Input is interleaved i16
//! samples; callers convert from their capture format first.

// Some helpers are only used by the WASAPI/PipeWire backends.
#![allow(dead_code)]

use crate::capture::{write_header, DATA_SOUND, HEADER_SIZE, OPUS_RATE};
use std::net::UdpSocket;

pub const RATE: u32 = OPUS_RATE;
pub const FRAME_MS: usize = 20;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES: usize = RATE as usize / 1000 * FRAME_MS;
pub const CHANNELS: usize = 2;

fn checked<T>(result: opus::Result<T>) -> anyhow::Result<T> {
    result.map_err(|e| anyhow::anyhow!("{}", e))
}

pub struct OpusEncoder {
    encoder: opus::Encoder,
    pending: Vec<i16>,
    scratch: Vec<i16>,
    packet: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(bitrate: i32) -> anyhow::Result<Self> {
        let mut encoder = checked(opus::Encoder::new(
            RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        ))?;
        checked(encoder.set_bitrate(opus::Bitrate::Bits(bitrate)))?;
        checked(encoder.set_vbr(true))?;

        Ok(Self {
            encoder,
            pending: Vec::with_capacity(FRAME_SAMPLES * CHANNELS),
            scratch: Vec::new(),
            // Far above the largest 20 ms packet at this bitrate.
            packet: vec![0u8; 4000],
        })
    }

    /// Push interleaved i16 samples, sending every completed 20 ms frame.
    pub fn push(
        &mut self,
        samples: &[i16],
        socket: &UdpSocket,
        seq: &mut u32,
        buf: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        let frame_len = FRAME_SAMPLES * CHANNELS;

        for &sample in samples {
            self.pending.push(sample);

            if self.pending.len() >= frame_len {
                let encoded = checked(self.encoder.encode(&self.pending[..frame_len], &mut self.packet))?;
                self.pending.drain(..frame_len);

                *seq = seq.wrapping_add(1);
                let size = HEADER_SIZE + encoded;

                if buf.len() < size {
                    buf.resize(size, 0);
                }

                write_header(buf, DATA_SOUND, *seq);
                buf[HEADER_SIZE..size].copy_from_slice(&self.packet[..encoded]);
                socket.send(&buf[..size])?;
            }
        }

        Ok(())
    }

    /// Push little-endian i16 bytes, as produced by WASAPI and PipeWire.
    pub fn push_bytes(
        &mut self,
        bytes: &[u8],
        socket: &UdpSocket,
        seq: &mut u32,
        buf: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        self.scratch.clear();
        self.scratch.reserve(bytes.len() / 2);

        for pair in bytes.chunks_exact(2) {
            self.scratch.push(i16::from_le_bytes([pair[0], pair[1]]));
        }

        // Avoid borrowing `self.scratch` and `self` mutably at once.
        let scratch = std::mem::take(&mut self.scratch);
        let result = self.push(&scratch, socket, seq, buf);
        self.scratch = scratch;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn encodes_complete_20ms_frames() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.connect(receiver.local_addr().unwrap()).unwrap();

        let mut encoder = OpusEncoder::new(96_000).unwrap();
        let mut seq = 0;
        let mut buf = Vec::new();

        // One full frame plus half of the next.
        let samples = vec![1000i16; FRAME_SAMPLES * CHANNELS + FRAME_SAMPLES];
        encoder.push(&samples, &sender, &mut seq, &mut buf).unwrap();

        assert_eq!(seq, 1, "one packet for the first frame, none for the rest");

        let mut packet = [0u8; 4096];
        let n = receiver.recv(&mut packet).unwrap();
        assert_eq!(packet[0], DATA_SOUND);
        assert_eq!(&packet[1..HEADER_SIZE], &1u32.to_le_bytes());

        let mut decoder = opus::Decoder::new(RATE, opus::Channels::Stereo).unwrap();
        let mut pcm = vec![0i16; 5760 * CHANNELS];
        let decoded = decoder
            .decode(&packet[HEADER_SIZE..n], &mut pcm, false)
            .unwrap();
        assert_eq!(decoded, FRAME_SAMPLES);
    }
}
