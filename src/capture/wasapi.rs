use crate::capture::{
    max_payload, send_config, send_silence, write_header, Output, CONFIG_INTERVAL, CODEC_OPUS,
    CODEC_PCM, HEADER_SIZE, OPUS_RATE, DATA_SOUND,
};
use crate::Args;
use std::collections::VecDeque;
use std::error;
use std::net::UdpSocket;
use std::time::Instant;
use wasapi::*;

type Res<T> = Result<T, Box<dyn error::Error>>;

/// wasapi errors are `Box<dyn Error>` (not `Send + Sync`), so they cannot
/// `?` into anyhow directly.
fn checked<T, E: std::fmt::Display>(result: Result<T, E>) -> anyhow::Result<T> {
    result.map_err(|e| anyhow::anyhow!("{}", e))
}

fn capture_loop(
    sender: &UdpSocket,
    rate: usize,
    bits: usize,
    server_buffer: u16,
    output: &mut Output,
    codec: &'static str,
) -> Res<()> {
    let device = get_default_device(&Direction::Render)?;

    let mut audio_client = device.get_iaudioclient()?;

    let desired_format = WaveFormat::new(bits, bits, &SampleType::Int, rate, 2, None);

    let blockalign = desired_format.get_blockalign();

    let (_def_time, min_time) = audio_client.get_periods()?;

    audio_client.initialize_client(
        &desired_format,
        min_time as i64,
        &Direction::Capture,
        &ShareMode::Shared,
        true,
    )?;

    let h_event = audio_client.set_get_eventhandle()?;

    let buffer_frame_count = audio_client.get_bufferframecount()?;

    let render_client = audio_client.get_audiocaptureclient()?;

    let mut sample_queue: VecDeque<u8> = VecDeque::with_capacity(
        100 * blockalign as usize * (1024 + 2 * buffer_frame_count as usize),
    );

    let payload = max_payload(bits / 8, 2);
    let mut buf = vec![0u8; HEADER_SIZE + payload];
    #[cfg(feature = "opus")]
    let mut bytes: Vec<u8> = Vec::new();
    let mut seq: u32 = 0;
    let mut last_config = Instant::now();

    audio_client.start_stream()?;

    let mut silence;

    loop {
        match output {
            Output::Pcm => {
                while sample_queue.len() > payload {
                    let size = HEADER_SIZE + payload;
                    silence = true;

                    for element in buf[HEADER_SIZE..size].iter_mut() {
                        *element = sample_queue.pop_front().unwrap();
                        if *element != 0 {
                            silence = false;
                        }
                    }

                    seq = seq.wrapping_add(1);

                    if silence {
                        send_silence(sender, &mut buf, seq, payload)?;
                        continue;
                    }

                    write_header(&mut buf, DATA_SOUND, seq);
                    sender.send(&buf[..size])?;
                }
            }
            #[cfg(feature = "opus")]
            Output::Opus(encoder) => {
                // Hand over whole i16 samples; the encoder does the framing.
                let complete = sample_queue.len() - sample_queue.len() % 2;
                bytes.clear();
                bytes.reserve(complete);

                for _ in 0..complete {
                    bytes.push(sample_queue.pop_front().unwrap());
                }

                encoder.push_bytes(&bytes, sender, &mut seq, &mut buf)?;
            }
        }

        if last_config.elapsed() >= CONFIG_INTERVAL {
            send_config(sender, bits, rate as u32, server_buffer, 2, codec)?;
            last_config = Instant::now();
        }

        render_client.read_from_device_to_deque(blockalign as usize, &mut sample_queue)?;
        if h_event.wait_for_event(600000).is_err() {
            eprintln!("timeout error, stopping capture");
            audio_client.stop_stream()?;
            break;
        }
    }
    Ok(())
}

pub fn stream(socket: UdpSocket, args: &Args) -> anyhow::Result<()> {
    initialize_mta()?;

    // The loopback capture carries the render endpoint's mix format. Use it to
    // pick the sample depth and rate instead of guessing.
    let device = checked(get_default_device(&Direction::Render))?;
    let mix = checked(checked(device.get_iaudioclient())?.get_mixformat())?;
    let source_bits = mix.get_bitspersample();
    let use_opus = args.opus();

    let bits = if use_opus {
        16
    } else if args.bits == 16 || args.bits == 32 {
        args.bits as usize
    } else if source_bits == 16 {
        16
    } else {
        32
    };

    let rate = if use_opus {
        OPUS_RATE as usize
    } else if args.rate != 0 {
        args.rate as usize
    } else {
        mix.get_samplespersec() as usize
    };

    let codec = if use_opus { CODEC_OPUS } else { CODEC_PCM };

    let mut output = {
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

    send_config(&socket, bits, rate as u32, args.buffer, 2, codec)?;

    socket.set_nonblocking(true)?;

    // Keep the sender alive across transient capture failures.
    loop {
        if let Err(e) = capture_loop(&socket, rate, bits, args.buffer, &mut output, codec) {
            eprintln!("capture error: {}", e);
        }

        // Re-announce the format in case the server restarted.
        let _ = send_config(&socket, bits, rate as u32, args.buffer, 2, codec);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
