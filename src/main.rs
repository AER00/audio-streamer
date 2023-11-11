use anyhow::anyhow;
use byteorder::{ByteOrder, LittleEndian};
use cfg_if::cfg_if;
use clap::Parser;
use kanal::{bounded, Receiver, Sender};
use pipewire as pw;
use pipewire::spa::format::{MediaSubtype, MediaType};
use pipewire::stream::StreamRef;
use pipewire::{properties, Context, MainLoop};
use pw::spa;
use spa::param::format_utils;
use spa::pod::Pod;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const CHUNK_SIZE: usize = 1024;

const DATA_SILENCE: u8 = 0;
const DATA_SOUND: u8 = 1;

cfg_if! {
    if #[cfg(target_pointer_width = "64")] {
        type Format = u64;
        const POINTER_BYTES: usize = 8;
    } else {
        type Format = u32;
        const POINTER_BYTES: usize = 4;
    }
}

/// Stream PC audio
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Sample rate
    #[arg(short, long, default_value_t = 0)]
    rate: u32,

    /// Remote ALSA's buffer size
    #[arg(short, long, default_value_t = 1024)]
    buffer: u16,

    /// Server address
    #[arg(short, long)]
    address: String,
}

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
}

fn stream_audio(receiver: Receiver<Format>, socket: &UdpSocket) -> anyhow::Result<()> {
    let mut buf = vec![0u8; CHUNK_SIZE + 1];
    let mut silence;
    let mut x;

    socket.set_write_timeout(Some(Duration::from_secs(1)))?;

    while !receiver.is_terminated() {
        silence = true;

        for bin in buf[1..].chunks_exact_mut(POINTER_BYTES) {
            x = receiver.recv()?;
            if x != 0 {
                silence = false;
            }
            bin.copy_from_slice(&x.to_le_bytes());
        }

        if silence {
            buf[0] = DATA_SILENCE;
            LittleEndian::write_u16(&mut buf[1..3], (CHUNK_SIZE + 1) as u16);
            socket.send(&buf[..3])?;
            continue;
        }

        buf[0] = DATA_SOUND;
        socket.send(&buf)?;
    }

    Ok(())
}

fn capture(stream: &StreamRef, sender: &Sender<Format>) -> anyhow::Result<()> {
    let mut buffer = stream.dequeue_buffer().ok_or(anyhow!("no buffers"))?;

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return Ok(());
    }

    let data = &mut datas[0];
    let end = data.chunk().size() as usize;

    let samples = data.data().ok_or(anyhow!("no data"))?;

    // transfer data in pointer size to reduce latency
    for bin in samples[..end].chunks_exact(POINTER_BYTES) {
        sender.send(Format::from_le_bytes(bin.try_into().unwrap()))?;
    }

    for bin in samples[..end].iter_mut() {
        *bin = 0;
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

pub fn main() -> anyhow::Result<()> {
    pw::init();

    let args = Args::parse();

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

    let socket = Arc::new(UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?);
    socket.connect(args.address)?;

    let socket2 = socket.clone();

    let (sender, receiver) = bounded(16384 / POINTER_BYTES);

    thread::spawn(move || {
        if let Err(e) = stream_audio(receiver, &socket) {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    });

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(move |_, id, user_data, param| {
            handle_param_change(id, user_data, param, &socket2, args.buffer)
        })
        .process(move |stream, _| {
            if let Err(e) = capture(stream, &sender) {
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
