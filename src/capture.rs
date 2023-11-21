#[cfg(target_os = "linux")]
mod pipewire;
#[cfg(target_os = "linux")]
pub use pipewire::stream;

#[cfg(target_os = "windows")]
mod wasapi;
#[cfg(target_os = "windows")]
pub use wasapi::stream;

use byteorder::{ByteOrder, LittleEndian};
use std::net::UdpSocket;

const CHUNK_SIZE: usize = 1024;

const DATA_SILENCE: u8 = 0;
const DATA_SOUND: u8 = 1;

#[cfg(target_os = "linux")]
pub fn is_silence(bin: &[u8]) -> bool {
    for x in bin {
        if *x != 0 {
            return false;
        }
    }
    return true;
}

pub fn send_silence(sender: &UdpSocket, buf: &mut Vec<u8>, size: usize) -> anyhow::Result<()> {
    buf[0] = DATA_SILENCE;
    LittleEndian::write_u16(&mut buf[1..3], size as u16);
    sender.send(&buf[..3])?;
    Ok(())
}
