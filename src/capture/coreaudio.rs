use crate::capture::send_silence;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::net::UdpSocket;
use std::sync::Arc;

use super::DATA_SOUND;

const SILENCE_THRESHOLD: f32 = 0.00001;
type S = f32;

fn is_silence(samples: &[S]) -> bool {
    samples.iter().all(|&s| s.abs() < SILENCE_THRESHOLD)
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
        .find(|device| device.name().unwrap() == name)
        .expect("Failed to find input device");

    let config: cpal::StreamConfig = input_device.default_input_config()?.into();

    // Create a pre-allocated buffer for direct UDP sending
    // Adding extra space for the message type byte
    let buffer_size = 1024 as usize;
    let mut send_buffer = vec![0u8; 1 + buffer_size * 4];

    let socket = Arc::new(socket);
    let socket2 = socket.clone();

    // Build input stream
    let input_stream = input_device.build_input_stream(
        &config,
        move |data: &[f32], _| {
            if is_silence(&data) {
                send_silence(&socket, &mut send_buffer, buffer_size * 4).unwrap();
                return;
            }
            send_buffer[0] = DATA_SOUND;

            // Write samples directly into the send buffer, no intermediary Vec
            let scale = 0x8000 as f32 * 0x10000 as f32; // 2147483648
            let mut counter = 0;
            for (i, &sample) in data[0..1024].iter().enumerate() {
                let pcm_i32 = (sample * scale).clamp(-2147483648.0, 2147483647.0) as i32;
                //let i = counter;
                send_buffer[1 + i * 4..1 + (i + 1) * 4].copy_from_slice(&pcm_i32.to_le_bytes());
                //counter += 1;
                //let i = counter;
                //send_buffer[1 + i * 4..1 + (i + 1) * 4].copy_from_slice(&pcm_i32.to_le_bytes());
                //counter += 1;
            }
            // Send directly from our pre-allocated buffer
            socket.send(&send_buffer[..1 + buffer_size * 4]).unwrap();
        },
        |err| eprintln!("Stream error: {}", err),
        None,
    )?;

    // Send initial configuration message
    socket2
        .send(format!("\x02{} {} {}", 32, config.sample_rate.0, args.buffer).as_ref())
        .unwrap();

    input_stream.play()?;
    // sleep for infinite time
    std::thread::sleep(std::time::Duration::MAX);

    Ok(())
}
