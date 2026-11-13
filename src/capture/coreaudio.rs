use crate::capture::{
    max_payload, send_config, send_silence, write_header, Output, CONFIG_INTERVAL, CODEC_OPUS,
    CODEC_PCM, HEADER_SIZE, DATA_SOUND,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize};
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Instant;

const SILENCE_THRESHOLD: f32 = 0.00001;
const SCALE: f32 = 2147483648.0;

/// A capture sample format that goes on the wire without conversion.
trait WireSample: SizedSample + Send + 'static {
    const BITS: usize;
    const BYTES: usize;

    fn is_silent(&self) -> bool;
    fn write_le(&self, dst: &mut [u8]);
    #[cfg(feature = "opus")]
    fn to_i16(&self) -> i16;
}

impl WireSample for f32 {
    const BITS: usize = 32;
    const BYTES: usize = 4;

    fn is_silent(&self) -> bool {
        self.abs() < SILENCE_THRESHOLD
    }

    fn write_le(&self, dst: &mut [u8]) {
        let pcm = (self * SCALE).clamp(-2147483648.0, 2147483647.0) as i32;
        dst.copy_from_slice(&pcm.to_le_bytes());
    }

    #[cfg(feature = "opus")]
    fn to_i16(&self) -> i16 {
        (self * 32767.0).clamp(-32768.0, 32767.0) as i16
    }
}

impl WireSample for i16 {
    const BITS: usize = 16;
    const BYTES: usize = 2;

    fn is_silent(&self) -> bool {
        *self == 0
    }

    fn write_le(&self, dst: &mut [u8]) {
        dst.copy_from_slice(&self.to_le_bytes());
    }

    #[cfg(feature = "opus")]
    fn to_i16(&self) -> i16 {
        *self
    }
}

fn run<T: WireSample>(
    device: &cpal::Device,
    config: &StreamConfig,
    socket: Arc<UdpSocket>,
    server_buffer: u16,
    mut sink: Output,
    codec: &'static str,
) -> anyhow::Result<()> {
    let sample_rate = config.sample_rate.0;
    let channels = config.channels as usize;
    let payload = max_payload(T::BYTES, channels);
    let frames_per_packet = payload / (T::BYTES * channels).max(1);

    // Opus always decodes to 16-bit on the server, whatever we capture in.
    let wire_bits = if codec == CODEC_OPUS { 16 } else { T::BITS };

    // Reused across callbacks; after the first packet this never reallocates
    let mut send_buffer: Vec<u8> = Vec::with_capacity(HEADER_SIZE + payload);

    let stream_socket = socket.clone();
    let mut seq: u32 = 0;
    let mut last_config = Instant::now();
    let mut logged_frames = false;
    #[cfg(feature = "opus")]
    let mut pcm: Vec<i16> = Vec::new();

    let input_stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            if !logged_frames {
                eprintln!("capture buffer: {} frames", data.len());
                logged_frames = true;
            }

            match &mut sink {
                Output::Pcm => {
                    for chunk in data.chunks(frames_per_packet) {
                        seq = seq.wrapping_add(1);

                        if chunk.iter().all(|sample| sample.is_silent()) {
                            let _ = send_silence(
                                &stream_socket,
                                &mut send_buffer,
                                seq,
                                chunk.len() * T::BYTES,
                            );
                            continue;
                        }

                        let size = HEADER_SIZE + chunk.len() * T::BYTES;

                        if send_buffer.len() < size {
                            send_buffer.resize(size, 0);
                        }

                        write_header(&mut send_buffer, DATA_SOUND, seq);

                        for (i, sample) in chunk.iter().enumerate() {
                            sample.write_le(
                                &mut send_buffer[HEADER_SIZE + i * T::BYTES
                                    ..HEADER_SIZE + (i + 1) * T::BYTES],
                            );
                        }

                        let _ = stream_socket.send(&send_buffer[..size]);
                    }
                }
                #[cfg(feature = "opus")]
                Output::Opus(encoder) => {
                    pcm.clear();
                    pcm.extend(data.iter().map(|sample| sample.to_i16()));
                    let _ = encoder.push(&pcm, &stream_socket, &mut seq, &mut send_buffer);
                }
            }

            if last_config.elapsed() >= CONFIG_INTERVAL {
                let _ = send_config(
                    &stream_socket,
                    wire_bits,
                    sample_rate,
                    server_buffer,
                    channels,
                    codec,
                );
                last_config = Instant::now();
            }
        },
        |err| eprintln!("Stream error: {}", err),
        None,
    )?;

    // Send initial configuration message
    let _ = send_config(
        &socket,
        wire_bits,
        sample_rate,
        server_buffer,
        channels,
        codec,
    );

    input_stream.play()?;
    // sleep for infinite time
    std::thread::sleep(std::time::Duration::MAX);

    Ok(())
}

pub fn stream(socket: UdpSocket, args: &crate::Args) -> anyhow::Result<()> {
    let host = cpal::default_host();
    // Find input device matching name
    let input_devices = host.input_devices()?;
    for device in input_devices {
        println!("{}", device.name()?);
    }

    let name = "BlackHole 2ch";
    //let name = "Urządzenie zespolone";

    let input_device = host
        .input_devices()?
        .find(|device| device.name().map(|n| n == name).unwrap_or(false))
        .expect("Failed to find input device");

    let supported = input_device.default_input_config()?;
    let source_format = supported.sample_format();
    let mut config: StreamConfig = supported.config();
    let use_opus = args.opus();

    // Opus runs at 48 kHz.
    #[cfg(feature = "opus")]
    if use_opus {
        config.sample_rate = cpal::SampleRate(crate::codec::RATE);
    }

    // A fixed callback size cuts latency; clamp it to what the device allows.
    if args.frames != 0 {
        if let SupportedBufferSize::Range { min, max } = *supported.buffer_size() {
            config.buffer_size = BufferSize::Fixed(args.frames.clamp(min, max));
        }
    }

    // Keep 16-bit sources at 16 bits instead of up-converting to 32.
    // Opus still captures in the device's native format and converts in the
    // callback: not every device (BlackHole, for one) accepts i16 streams.
    let bits = if use_opus {
        if source_format == SampleFormat::I16 {
            16
        } else {
            32
        }
    } else if args.bits == 16 || (args.bits != 32 && source_format == SampleFormat::I16) {
        16
    } else {
        32
    };

    let codec = if use_opus { CODEC_OPUS } else { CODEC_PCM };

    let sink = {
        #[cfg(feature = "opus")]
        {
            if use_opus {
                Output::Opus(crate::codec::OpusEncoder::new(args.bitrate)?)
            } else {
                Output::Pcm
            }
        }
        #[cfg(not(feature = "opus"))]
        {
            Output::Pcm
        }
    };

    let socket = Arc::new(socket);

    match bits {
        16 => run::<i16>(&input_device, &config, socket, args.buffer, sink, codec),
        _ => run::<f32>(&input_device, &config, socket, args.buffer, sink, codec),
    }
}
