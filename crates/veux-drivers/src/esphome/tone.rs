//! Test tones on an ESPHome media player.
//!
//! veux makes a short stereo FLAC, serves it once over HTTP, and tells the
//! media player to play its URL. The media pipeline of the device is
//! stereo, so a silent channel gives a left or right tone.
//!
//! FLAC, not WAV: an ESPHome media player decodes only the formats that its
//! config asks for, and FLAC is the default. The frames are not compressed
//! (VERBATIM subframes), which keeps the encoder small.
//!
//! The device must reach the machine that runs veux on `PORT`. A host
//! firewall must allow that port.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::metrics::{Tone, ToneChannel};

/// The TCP port that serves the tone file. It is fixed, so that a host
/// firewall can allow it.
pub const PORT: u16 = 8790;
pub const SECONDS: f64 = 3.0;
const RATE: u32 = 48_000;
/// -18 dBFS: clear, and not loud at full media volume.
const AMPLITUDE: f64 = 0.125;
/// The fade at each end, so the tone starts and stops without a click.
const FADE: f64 = 0.02;
/// How long the device has to fetch the file.
const FETCH_LIMIT: Duration = Duration::from_secs(10);

/// Frames of this many samples. 4096 is the usual FLAC block size.
const BLOCK: usize = 4096;

/// The samples of the tone, as (left, right) pairs.
fn samples(tone: Tone, seconds: f64) -> Vec<(i16, i16)> {
    let frames = (f64::from(RATE) * seconds) as u32;
    let (left, right) = match tone.channel {
        ToneChannel::Left => (true, false),
        ToneChannel::Right => (false, true),
        ToneChannel::Both => (true, true),
    };
    (0..frames)
        .map(|i| {
            let t = f64::from(i) / f64::from(RATE);
            let fade = (t / FADE).min((seconds - t) / FADE).clamp(0.0, 1.0);
            let x = AMPLITUDE
                * fade
                * (2.0 * std::f64::consts::PI * f64::from(tone.frequency_hz) * t).sin();
            let s = (x * f64::from(i16::MAX)) as i16;
            (if left { s } else { 0 }, if right { s } else { 0 })
        })
        .collect()
}

/// A 16-bit stereo FLAC of the tone at 48 kHz.
pub fn flac(tone: Tone, seconds: f64) -> Vec<u8> {
    let samples = samples(tone, seconds);
    let mut out = b"fLaC".to_vec();
    // The one metadata block, STREAMINFO (type 0), is the last one.
    out.push(0x80);
    out.extend_from_slice(&34u32.to_be_bytes()[1..]);
    out.extend_from_slice(&(BLOCK as u16).to_be_bytes()); // minimum block size
    out.extend_from_slice(&(BLOCK as u16).to_be_bytes()); // maximum block size
    out.extend_from_slice(&[0; 6]); // frame sizes: unknown
    let info: u64 = (u64::from(RATE) << 44) | (1 << 41) | (15 << 36) | samples.len() as u64;
    out.extend_from_slice(&info.to_be_bytes());
    out.extend_from_slice(&[0; 16]); // MD5: not computed
    for (number, block) in samples.chunks(BLOCK).enumerate() {
        let start = out.len();
        // Sync code, fixed block size; the block size follows the frame
        // number as 16 bits (code 7); 48 kHz (code 10).
        out.extend_from_slice(&[0xFF, 0xF8, 0x7A]);
        // Two independent channels (code 1), 16 bits (code 4).
        out.push(0x18);
        out.extend_from_slice(&utf8_number(number as u32));
        out.extend_from_slice(&((block.len() - 1) as u16).to_be_bytes());
        let crc = crc8(&out[start..]);
        out.push(crc);
        for channel in 0..2 {
            out.push(0x02); // VERBATIM subframe, no wasted bits
            for &(l, r) in block {
                let s = if channel == 0 { l } else { r };
                out.extend_from_slice(&s.to_be_bytes());
            }
        }
        let crc = crc16(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out
}

/// The frame number in the UTF-8 style coding of FLAC.
fn utf8_number(n: u32) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let mut tail = Vec::new();
    let mut rest = n;
    let mut first_bits = 6;
    while rest >= 1 << first_bits {
        tail.push(0x80 | (rest & 0x3F) as u8);
        rest >>= 6;
        first_bits -= 1;
    }
    let lead = (0xFF00u16 >> (tail.len() + 1)) as u8;
    let mut out = vec![lead | rest as u8];
    out.extend(tail.iter().rev());
    out
}

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut crc, &b| {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
        crc
    })
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut crc, &b| {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// The local address that `device` sees: the address of the interface on
/// the route to it. No packet is sent.
pub fn local_ip_for(device: &str) -> std::io::Result<IpAddr> {
    let target = device
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other(format!("cannot resolve {device}")))?;
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.connect(target)?;
    Ok(socket.local_addr()?.ip())
}

/// Serve `body` to the first GET on `listener`, in a thread. The receiver
/// gets the result when the file is sent, or when the device did not fetch
/// it in time.
pub fn serve_once(listener: TcpListener, body: Vec<u8>) -> Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(serve(listener, &body));
    });
    rx
}

fn serve(listener: TcpListener, body: &[u8]) -> Result<(), String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + FETCH_LIMIT;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0u8; 1024];
                let n = stream.read(&mut request).unwrap_or(0);
                if !request[..n].starts_with(b"GET ") {
                    continue;
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                return stream
                    .write_all(header.as_bytes())
                    .and_then(|()| stream.write_all(body))
                    .map_err(|e| format!("sending the tone: {e}"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    return Err(format!(
                        "the device did not fetch the tone; check that the firewall \
                         of this host allows TCP port {PORT}"
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_have_one_silent_channel_and_a_fade() {
        let tone = Tone {
            frequency_hz: 440,
            channel: ToneChannel::Right,
        };
        let s = samples(tone, 0.1);
        assert_eq!(s.len(), 4800);
        assert!(s.iter().all(|&(l, _)| l == 0));
        let peak = s.iter().map(|&(_, r)| r.unsigned_abs()).max().unwrap();
        assert!((4000..=4096).contains(&peak), "{peak}");
        assert_eq!(s[0], (0, 0), "the fade starts at zero");
    }

    #[test]
    fn flac_has_streaminfo_and_checked_frames() {
        let tone = Tone {
            frequency_hz: 440,
            channel: ToneChannel::Both,
        };
        let f = flac(tone, 0.1);
        assert_eq!(&f[..4], b"fLaC");
        assert_eq!(&f[4..8], &[0x80, 0, 0, 34]);
        // Two frames: 4096 samples and 704 samples.
        let first = 8 + 34;
        assert_eq!(&f[first..first + 5], &[0xFF, 0xF8, 0x7A, 0x18, 0]);
        assert_eq!(&f[first + 5..first + 7], &4095u16.to_be_bytes());
        let frame = 8 + 2 + 4 * 4096 + 2;
        let second = first + frame;
        assert_eq!(&f[second..second + 5], &[0xFF, 0xF8, 0x7A, 0x18, 1]);
        assert_eq!(f.len(), second + 8 + 2 + 4 * 704 + 2);
        // A CRC over data that ends with its own CRC gives zero.
        assert_eq!(crc8(&f[first..first + 8]), 0);
        assert_eq!(crc16(&f[first..second]), 0);
    }

    #[test]
    fn frame_numbers_use_the_utf8_coding() {
        assert_eq!(utf8_number(5), [5]);
        assert_eq!(utf8_number(0x80), [0xC2, 0x80]);
        assert_eq!(utf8_number(0x7FF), [0xDF, 0xBF]);
        assert_eq!(utf8_number(0x800), [0xE0, 0xA0, 0x80]);
    }

    #[test]
    fn serves_the_file_to_one_get() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let done = serve_once(listener, b"tone".to_vec());
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"GET /tone.wav HTTP/1.1\r\n\r\n").unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK"));
        assert!(reply.ends_with("\r\n\r\ntone"));
        assert_eq!(done.recv().unwrap(), Ok(()));
    }

    #[test]
    fn local_ip_is_on_the_route_to_the_device() {
        assert!(local_ip_for("127.0.0.1:6053").unwrap().is_loopback());
    }
}
