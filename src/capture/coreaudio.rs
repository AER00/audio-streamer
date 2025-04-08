use crate::capture::send_silence;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapRb,
};
use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};

const SAMPLE_RATE: f32 = 44100.0;
const DATA_SOUND: u8 = 1;
const SILENCE_THRESHOLD: f32 = 0.00001;
type S = f32; // Sample type (f32)

fn is_silence(samples: &[S]) -> bool {
    samples.iter().all(|&s| s.abs() < SILENCE_THRESHOLD)
}

pub fn stream(socket: UdpSocket, args: &crate::Args) -> anyhow::Result<()> {
    let host = cpal::default_host();
    // Find input device matching name
    let input_device = host
        .input_devices()?
        .find(|device| device.name().unwrap() == "BlackHole 2ch")
        .expect("Failed to find input device");

    let config: cpal::StreamConfig = input_device.default_input_config()?.into();
    let latency_samples = 1024 as usize;

    // Using a larger ring buffer for smooth operation
    let ring = HeapRb::<f32>::new(latency_samples * 4);
    let (mut producer, mut consumer) = ring.split();

    // Pre-fill the ring buffer with silence
    for _ in 0..latency_samples {
        producer.try_push(0.0).unwrap();
    }

    // Create a pre-allocated buffer for direct UDP sending
    // Adding extra space for the message type byte
    let buffer_size = 1024 as usize;
    let mut send_buffer = vec![0u8; 1 + buffer_size * 4];

    // Create a pre-allocated workspace buffer for silence checking
    let mut sample_workspace = Vec::with_capacity(buffer_size);
    unsafe {
        sample_workspace.set_len(buffer_size);
    }

    // Build input stream
    let input_stream = input_device.build_input_stream(
        &config,
        move |data: &[f32], _| {
            // Try to push all samples without checking individual results
            // This can be more efficient when producer has enough space
            println!("Pushing samples, {}", data.len());
            let _ = producer.push_slice(data);
        },
        |err| eprintln!("Stream error: {}", err),
        None,
    )?;

    input_stream.play()?;

    // Send initial configuration message
    socket
        .send(format!("\x02{} {} {}", 32, config.sample_rate.0, args.buffer).as_ref())
        .unwrap();

    loop {
        // Try to fill our workspace buffer with exactly buffer_size samples
        let mut samples_read = 0;
        while samples_read < buffer_size {
            // Try to pop in bulk for better performance
            let count = consumer.pop_slice(&mut sample_workspace[samples_read..buffer_size]);
            if count == 0 {
                // No samples available yet, might want to add a small sleep here
                // to avoid busy-waiting, depending on your latency requirements
                std::thread::yield_now();
                continue;
            }
            samples_read += count;
        }

        // Check if the samples are silence
        if is_silence(&sample_workspace[0..buffer_size]) {
            send_silence(&socket, &mut send_buffer, buffer_size * 4)?;
            continue;
        }

        // send_buffer[0] = DATA_SOUND; // Set message type once

        // // Write samples directly into the send buffer, no intermediary Vec
        // let scale = 0x8000 as f32 * 0x10000 as f32; // 2147483648
        // for (i, &sample) in sample_workspace[0..buffer_size].iter().enumerate() {
        //     let pcm_i32 = (sample * scale).clamp(-2147483648.0, 2147483647.0) as i32;
        //     send_buffer[1 + i * 4..1 + (i + 1) * 4].copy_from_slice(&pcm_i32.to_le_bytes());
        // }

        // // Send directly from our pre-allocated buffer
        // socket.send(&send_buffer[..1 + buffer_size * 4]).unwrap();
    }
}
