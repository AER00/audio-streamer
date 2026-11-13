use crate::capture::{
    is_silence, max_payload, send_config, send_silence, write_header, CONFIG_INTERVAL, HEADER_SIZE,
    MAX_PAYLOAD, DATA_SOUND,
};
use crate::Args;
use anyhow::anyhow;
use pipewire as pw;
use pipewire::stream::StreamRef;
use pipewire::{properties, Context, MainLoop};
use pw::spa;
use spa::format::{MediaSubtype, MediaType};
use spa::param::audio::AudioFormat;
use spa::param::format_utils;
use spa::pod::Pod;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
}

fn capture(
    stream: &StreamRef,
    sender: &UdpSocket,
    buf: &mut Vec<u8>,
    seq: &mut u32,
    payload: usize,
) -> anyhow::Result<()> {
    let mut buffer = stream.dequeue_buffer().ok_or(anyhow!("no buffers"))?;

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return Ok(());
    }

    let data = &mut datas[0];
    let end = data.chunk().size() as usize;

    let samples = data.data().ok_or(anyhow!("no data"))?;

    for bin in samples[..end].chunks(payload) {
        *seq = seq.wrapping_add(1);

        if is_silence(bin) {
            send_silence(sender, buf, *seq, bin.len())?;
            continue;
        }

        let size = HEADER_SIZE + bin.len();
        if buf.len() < size {
            buf.resize(size, 0);
        }
        write_header(buf, DATA_SOUND, *seq);
        buf[HEADER_SIZE..size].copy_from_slice(bin);
        sender.send(&buf[..size])?;
    }

    Ok(())
}

fn handle_param_change(
    id: u32,
    user_data: &mut UserData,
    param: Option<&Pod>,
    socket: &UdpSocket,
    buf_size: u16,
) {
    // NULL means to clear the format
    let Some(param) = param else {
        return;
    };
    if id != spa::param::ParamType::Format.as_raw() {
        return;
    }

    let (media_type, media_subtype) = match format_utils::parse_format(param) {
        Ok(v) => v,
        Err(_) => return,
    };

    // only accept raw audio
    if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
        return;
    }

    // call a helper function to parse the format for us.
    user_data
        .format
        .parse(param)
        .expect("Failed to parse param changed to AudioInfoRaw");

    let bits = match user_data.format.format() {
        AudioFormat::S16LE => 16,
        _ => 32,
    };
    let channels = user_data.format.channels().max(1) as usize;

    let _ = send_config(socket, bits, user_data.format.rate(), buf_size, channels);

    println!(
        "capturing rate:{} channels:{} bits:{}",
        user_data.format.rate(),
        user_data.format.channels(),
        bits
    );
}

pub fn stream(socket: UdpSocket, args: &Args) -> anyhow::Result<()> {
    pw::init();

    let mainloop = MainLoop::new()?;
    let context = Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let data = UserData {
        format: Default::default(),
    };

    let props = if args.frames != 0 {
        let rate = if args.rate != 0 { args.rate } else { 48_000 };
        let latency = format!("{}/{}", args.frames, rate);
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            *pw::keys::NODE_LATENCY => latency,
        }
    } else {
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::STREAM_CAPTURE_SINK => "true",
        }
    };

    let stream = pw::stream::Stream::new(&core, "audio-capture", props)?;

    let socket = Arc::new(socket);
    let socket2 = socket.clone();
    let buffer_size = args.buffer;
    let mut buf = vec![0u8; HEADER_SIZE + MAX_PAYLOAD];
    let mut seq: u32 = 0;
    let mut last_config = Instant::now();
    let mut last_error = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(move |_, id, user_data, param| {
            handle_param_change(id, user_data, param, &socket2, buffer_size)
        })
        .process(move |stream, user_data| {
            let sample_bytes = match user_data.format.format() {
                AudioFormat::S16LE => 2,
                _ => 4,
            };
            let channels = user_data.format.channels().max(1) as usize;
            let payload = max_payload(sample_bytes, channels);

            if let Err(e) = capture(stream, &socket, &mut buf, &mut seq, payload) {
                // A transient send error must not kill the sender.
                if last_error.elapsed() >= Duration::from_secs(1) {
                    eprintln!("{}", e);
                    last_error = Instant::now();
                }
            }

            if user_data.format.rate() != 0 && last_config.elapsed() >= CONFIG_INTERVAL {
                let bits = if sample_bytes == 2 { 16 } else { 32 };
                let _ = send_config(&socket, bits, user_data.format.rate(), buffer_size, channels);
                last_config = Instant::now();
            }
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    // Request the native depth: 16-bit sources stay 16-bit with --bits 16,
    // otherwise the existing 32-bit path is kept.
    audio_info.set_format(if args.bits == 16 {
        AudioFormat::S16LE
    } else {
        AudioFormat::S32LE
    });

    if args.rate != 0 {
        audio_info.set_rate(args.rate);
    }

    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };

    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )?
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).ok_or(anyhow!("cannot create Pod from_bytes"))?];

    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;

    stream.connect(spa::Direction::Input, None, flags, &mut params)?;

    mainloop.run();

    Ok(())
}
