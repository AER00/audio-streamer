use crate::capture::{
    max_payload, send_config, send_silence, write_header, CONFIG_INTERVAL, HEADER_SIZE, DATA_SOUND,
};
use crate::Args;
use std::collections::VecDeque;
use std::error;
use std::net::UdpSocket;
use std::time::Instant;
use wasapi::*;

type Res<T> = Result<T, Box<dyn error::Error>>;

fn capture_loop(sender: &UdpSocket, rate: usize, bits: usize, server_buffer: u16) -> Res<()> {
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
    let mut seq: u32 = 0;
    let mut last_config = Instant::now();

    audio_client.start_stream()?;

    let mut silence;

    loop {
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

        if last_config.elapsed() >= CONFIG_INTERVAL {
            send_config(sender, bits, rate as u32, server_buffer, 2)?;
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
    let device = get_default_device(&Direction::Render)?;
    let mix = device.get_iaudioclient()?.get_mixformat()?;
    let source_bits = mix.get_bitspersample();

    let bits = if args.bits == 16 || args.bits == 32 {
        args.bits as usize
    } else if source_bits == 16 {
        16
    } else {
        32
    };

    let rate = if args.rate != 0 {
        args.rate as usize
    } else {
        mix.get_samplespersec() as usize
    };

    send_config(&socket, bits, rate as u32, args.buffer, 2)?;

    socket.set_nonblocking(true)?;

    if let Err(e) = capture_loop(&socket, rate, bits, args.buffer) {
        // panic!("{}", e);
        eprintln!("{}", e);
    }

    Ok(())
}
