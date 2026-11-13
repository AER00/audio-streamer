use crate::capture::{send_config, send_silence, DATA_SOUND};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample, StreamConfig};
use std::net::UdpSocket;
use std::sync::Arc;

const SILENCE_THRESHOLD: f32 = 0.00001;
const SCALE: f32 = 2147483648.0;

/// A capture sample format that goes on the wire without conversion.
trait WireSample: SizedSample + Send + 'static {
    const BITS: usize;
    const BYTES: usize;

    fn is_silent(&self) -> bool;
    fn write_le(&self, dst: &mut [u8]);
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
}

fn run<T: WireSample>(
    device: &cpal::Device,
    config: &StreamConfig,
    socket: Arc<UdpSocket>,
    server_buffer: u16,
) -> anyhow::Result<()> {
    // Grow on demand; after the first callback this never reallocates
    let mut send_buffer: Vec<u8> = Vec::new();

    let stream_socket = socket.clone();

    let input_stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let size = 1 + data.len() * T::BYTES;

            if send_buffer.len() < size {
                send_buffer.resize(size, 0);
            }

            if data.iter().all(|sample| sample.is_silent()) {
                let _ = send_silence(&stream_socket, &mut send_buffer, data.len() * T::BYTES + 1);
                return;
            }

            send_buffer[0] = DATA_SOUND;

            for (i, sample) in data.iter().enumerate() {
                sample.write_le(&mut send_buffer[1 + i * T::BYTES..1 + (i + 1) * T::BYTES]);
            }

            let _ = stream_socket.send(&send_buffer[..size]);
        },
        |err| eprintln!("Stream error: {}", err),
        None,
    )?;

    // Send initial configuration message
    let _ = send_config(&socket, T::BITS, config.sample_rate.0, server_buffer);

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
    let config: StreamConfig = supported.config();

    // Keep 16-bit sources at 16 bits instead of up-converting to 32
    let bits = if args.bits == 16 || (args.bits != 32 && source_format == SampleFormat::I16) {
        16
    } else {
        32
    };

    let socket = Arc::new(socket);

    match bits {
        16 => run::<i16>(&input_device, &config, socket, args.buffer),
        _ => run::<f32>(&input_device, &config, socket, args.buffer),
    }
}
