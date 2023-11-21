use crate::capture::{send_silence, CHUNK_SIZE, DATA_SOUND};
use crate::Args;
use std::collections::VecDeque;
use std::error;
use std::net::UdpSocket;
use wasapi::*;

type Res<T> = Result<T, Box<dyn error::Error>>;

fn capture_loop(sender: &UdpSocket, rate: usize) -> Res<()> {
    let device = get_default_device(&Direction::Render)?;

    let mut audio_client = device.get_iaudioclient()?;

    let desired_format = WaveFormat::new(32, 32, &SampleType::Int, rate, 2, None);

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

    let mut buf = vec![0u8; 16384];
    buf[0] = 1;

    audio_client.start_stream()?;

    let mut silence;

    loop {
        while sample_queue.len() > (blockalign as usize * CHUNK_SIZE as usize) {
            let size = blockalign as usize * CHUNK_SIZE as usize + 1;
            silence = true;

            for element in buf[1..size].iter_mut() {
                *element = sample_queue.pop_front().unwrap();
                if *element != 0 {
                    silence = false;
                }
            }

            if silence {
                send_silence(sender, &mut buf, size)?;
                continue;
            }

            buf[0] = DATA_SOUND;
            sender.send(&buf[..size])?;
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

    let mut rate = args.rate as usize;
    if rate == 0 {
        rate = 44100;
    }

    socket
        .send(format!("\x02{} {} {}", 32, rate, args.buffer).as_ref())
        .unwrap();

    socket.set_nonblocking(true)?;

    if let Err(e) = capture_loop(&socket, rate) {
        // panic!("{}", e);
        eprintln!("{}", e);
    }

    Ok(())
}
