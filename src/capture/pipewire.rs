use crate::capture::{is_silence, send_silence, CHUNK_SIZE, DATA_SOUND};
use crate::Args;
use anyhow::anyhow;
use pipewire as pw;
use pipewire::stream::StreamRef;
use pipewire::{properties, Context, MainLoop};
use pw::spa;
use spa::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use std::net::UdpSocket;
use std::sync::Arc;

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
}

fn capture(stream: &StreamRef, sender: &UdpSocket, buf: &mut Vec<u8>) -> anyhow::Result<()> {
    let mut buffer = stream.dequeue_buffer().ok_or(anyhow!("no buffers"))?;

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return Ok(());
    }

    let data = &mut datas[0];
    let end = data.chunk().size() as usize;

    let samples = data.data().ok_or(anyhow!("no data"))?;

    for bin in samples[..end].chunks(CHUNK_SIZE) {
        let size = bin.len() + 1;

        if is_silence(bin) {
            send_silence(sender, buf, size)?;
            continue;
        }

        buf[0] = DATA_SOUND;
        buf[1..size].copy_from_slice(bin);
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

    socket
        .send(format!("\x02{} {} {}", 32, user_data.format.rate(), buf_size).as_ref())
        .unwrap();

    println!(
        "capturing rate:{} channels:{}",
        user_data.format.rate(),
        user_data.format.channels()
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

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::STREAM_CAPTURE_SINK => "true",
    };

    let stream = pw::stream::Stream::new(&core, "audio-capture", props)?;

    let socket = Arc::new(socket);
    let socket2 = socket.clone();
    let buffer_size = args.buffer;
    let mut buf = vec![0u8; 2048];

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(move |_, id, user_data, param| {
            handle_param_change(id, user_data, param, &socket2, buffer_size)
        })
        .process(move |stream, _| {
            if let Err(e) = capture(stream, &socket, &mut buf) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::S32LE);

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
