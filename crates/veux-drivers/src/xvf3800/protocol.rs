//! XVF3800 vendor control protocol. See `docs/devices/xvf3800.md`.

use std::thread::sleep;
use std::time::Duration;

use thiserror::Error;
use veux_model::Value;

/// Maximum bytes in one control transfer.
pub const MAX_TRANSFER: usize = 64;

const READ_FLAG: u16 = 0x80;
const STATUS_SUCCESS: u8 = 0;
/// Both codes mean "not ready, ask again". 1 is from the older XMOS
/// protocol, 64 is `SERVICER_COMMAND_RETRY`.
const STATUS_RETRY: [u8; 2] = [1, 64];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    U8,
    Char,
    U16,
    I32,
    U32,
    F32,
    Radians,
}

impl DataType {
    pub fn size(self) -> usize {
        match self {
            DataType::U8 | DataType::Char => 1,
            DataType::U16 => 2,
            DataType::I32 | DataType::U32 | DataType::F32 | DataType::Radians => 4,
        }
    }

    fn is_float(self) -> bool {
        matches!(self, DataType::F32 | DataType::Radians)
    }

    fn int_range(self) -> (i64, i64) {
        match self {
            DataType::U8 | DataType::Char => (0, u8::MAX.into()),
            DataType::U16 => (0, u16::MAX.into()),
            DataType::I32 => (i32::MIN.into(), i32::MAX.into()),
            DataType::U32 => (0, u32::MAX.into()),
            DataType::F32 | DataType::Radians => (i64::MIN, i64::MAX),
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("transfer failed: {0}")]
    Transport(String),
    #[error("device returned status {0}")]
    Status(u8),
    #[error("device stayed busy after {0} retries")]
    Busy(u32),
    #[error("short read: expected {expected} bytes, got {got}")]
    ShortRead { expected: usize, got: usize },
    #[error("payload of {0} bytes exceeds the transfer limit")]
    TooLarge(usize),
    #[error("cannot encode {value} as {count} x {ty:?}")]
    Encode {
        value: String,
        ty: DataType,
        count: usize,
    },
}

/// Access to the vendor control requests on endpoint 0.
///
/// `value` is wValue and `index` is wIndex of the setup packet.
pub trait ControlPort {
    fn control_in(&mut self, value: u16, index: u16, buf: &mut [u8]) -> Result<usize, Error>;
    fn control_out(&mut self, value: u16, index: u16, data: &[u8]) -> Result<(), Error>;
}

impl<P: ControlPort + ?Sized> ControlPort for &mut P {
    fn control_in(&mut self, value: u16, index: u16, buf: &mut [u8]) -> Result<usize, Error> {
        (**self).control_in(value, index, buf)
    }

    fn control_out(&mut self, value: u16, index: u16, data: &[u8]) -> Result<(), Error> {
        (**self).control_out(value, index, data)
    }
}

pub struct Client<P> {
    port: P,
    retry_delay: Duration,
    max_retries: u32,
}

impl<P: ControlPort> Client<P> {
    pub fn new(port: P) -> Self {
        Self {
            port,
            retry_delay: Duration::from_millis(10),
            max_retries: 100,
        }
    }

    pub fn with_retry(mut self, delay: Duration, max_retries: u32) -> Self {
        self.retry_delay = delay;
        self.max_retries = max_retries;
        self
    }

    #[cfg(test)]
    pub(crate) fn port(&self) -> &P {
        &self.port
    }

    #[cfg(test)]
    pub(crate) fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }

    /// Read `len` payload bytes. The device puts a status byte first.
    pub fn read(&mut self, resource: u8, command: u8, len: usize) -> Result<Vec<u8>, Error> {
        if len + 1 > MAX_TRANSFER {
            return Err(Error::TooLarge(len));
        }
        let mut buf = vec![0; len + 1];
        for _ in 0..=self.max_retries {
            let got =
                self.port
                    .control_in(u16::from(command) | READ_FLAG, resource.into(), &mut buf)?;
            if got == 0 {
                return Err(Error::ShortRead {
                    expected: len + 1,
                    got,
                });
            }
            match buf[0] {
                STATUS_SUCCESS if got == len + 1 => return Ok(buf[1..].to_vec()),
                STATUS_SUCCESS => {
                    return Err(Error::ShortRead {
                        expected: len + 1,
                        got,
                    });
                }
                s if STATUS_RETRY.contains(&s) => sleep(self.retry_delay),
                s => return Err(Error::Status(s)),
            }
        }
        Err(Error::Busy(self.max_retries))
    }

    pub fn write(&mut self, resource: u8, command: u8, data: &[u8]) -> Result<(), Error> {
        if data.len() > MAX_TRANSFER {
            return Err(Error::TooLarge(data.len()));
        }
        self.port.control_out(command.into(), resource.into(), data)
    }
}

/// Convert payload bytes to a value. One numeric value gives `Int` or
/// `Float`. More than one gives `Vector`. `Char` gives `Text`.
pub fn decode(ty: DataType, count: usize, bytes: &[u8]) -> Value {
    if ty == DataType::Char {
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        return Value::Text(String::from_utf8_lossy(&bytes[..end]).into_owned());
    }
    let numbers: Vec<f64> = bytes
        .chunks_exact(ty.size())
        .map(|c| number(ty, c))
        .collect();
    match (count, ty.is_float()) {
        (1, true) => Value::Float(numbers[0]),
        // Integer types are at most 32 bits, so the f64 holds them exactly.
        (1, false) => Value::Int(numbers[0] as i64),
        _ => Value::Vector(numbers),
    }
}

fn number(ty: DataType, c: &[u8]) -> f64 {
    match ty {
        DataType::U8 | DataType::Char => c[0].into(),
        DataType::U16 => u16::from_le_bytes([c[0], c[1]]).into(),
        DataType::I32 => i32::from_le_bytes([c[0], c[1], c[2], c[3]]).into(),
        DataType::U32 => u32::from_le_bytes([c[0], c[1], c[2], c[3]]).into(),
        DataType::F32 | DataType::Radians => f32::from_le_bytes([c[0], c[1], c[2], c[3]]).into(),
    }
}

/// Convert a value to payload bytes. The inverse of [`decode`].
pub fn encode(ty: DataType, count: usize, value: &Value) -> Result<Vec<u8>, Error> {
    let fail = || Error::Encode {
        value: value.to_string(),
        ty,
        count,
    };
    let numbers: Vec<f64> = match value {
        Value::Int(v) if count == 1 => vec![*v as f64],
        Value::Float(v) if count == 1 => vec![*v],
        Value::Bool(v) if count == 1 && !ty.is_float() => vec![f64::from(u8::from(*v))],
        Value::Vector(v) if v.len() == count => v.clone(),
        Value::Text(s) if ty == DataType::Char && s.len() <= count => {
            let mut bytes = s.as_bytes().to_vec();
            bytes.resize(count, 0);
            return Ok(bytes);
        }
        _ => return Err(fail()),
    };
    let mut out = Vec::with_capacity(count * ty.size());
    for n in numbers {
        if ty.is_float() {
            out.extend_from_slice(&(n as f32).to_le_bytes());
            continue;
        }
        let (min, max) = ty.int_range();
        if n.fract() != 0.0 || n < min as f64 || n > max as f64 {
            return Err(fail());
        }
        let n = n as i64;
        match ty {
            DataType::U8 | DataType::Char => out.push(n as u8),
            DataType::U16 => out.extend_from_slice(&(n as u16).to_le_bytes()),
            DataType::I32 => out.extend_from_slice(&(n as i32).to_le_bytes()),
            DataType::U32 => out.extend_from_slice(&(n as u32).to_le_bytes()),
            DataType::F32 | DataType::Radians => unreachable!(),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Plays back device replies in order and records each request.
    #[derive(Default)]
    struct ScriptedPort {
        replies: VecDeque<Vec<u8>>,
        requests: Vec<(u16, u16, Vec<u8>)>,
    }

    impl ControlPort for ScriptedPort {
        fn control_in(&mut self, value: u16, index: u16, buf: &mut [u8]) -> Result<usize, Error> {
            self.requests.push((value, index, vec![0; buf.len()]));
            let reply = self.replies.pop_front().expect("no scripted reply");
            buf[..reply.len()].copy_from_slice(&reply);
            Ok(reply.len())
        }

        fn control_out(&mut self, value: u16, index: u16, data: &[u8]) -> Result<(), Error> {
            self.requests.push((value, index, data.to_vec()));
            Ok(())
        }
    }

    fn client(replies: &[&[u8]]) -> Client<ScriptedPort> {
        let port = ScriptedPort {
            replies: replies.iter().map(|r| r.to_vec()).collect(),
            ..Default::default()
        };
        Client::new(port).with_retry(Duration::ZERO, 3)
    }

    #[test]
    fn read_sets_flag_and_strips_status() {
        let mut c = client(&[&[0, 2, 1, 1]]);
        assert_eq!(c.read(48, 0, 3).unwrap(), [2, 1, 1]);
        let (value, index, buf) = &c.port.requests[0];
        assert_eq!((*value, *index, buf.len()), (0x80, 48, 4));
    }

    #[test]
    fn read_retries_on_busy_status() {
        let mut c = client(&[&[64, 0], &[1, 0], &[0, 7]]);
        assert_eq!(c.read(17, 10, 1).unwrap(), [7]);
        assert_eq!(c.port.requests.len(), 3);
    }

    #[test]
    fn read_gives_up_after_max_retries() {
        let mut c = client(&[&[64], &[64], &[64], &[64]]);
        assert!(matches!(c.read(17, 10, 1), Err(Error::Busy(3))));
    }

    #[test]
    fn read_reports_error_status() {
        let mut c = client(&[&[5]]);
        assert!(matches!(c.read(99, 1, 1), Err(Error::Status(5))));
    }

    #[test]
    fn read_rejects_short_payload() {
        let mut c = client(&[&[0, 1]]);
        assert!(matches!(c.read(48, 0, 3), Err(Error::ShortRead { .. })));
    }

    #[test]
    fn write_sends_payload_without_flag() {
        let mut c = client(&[]);
        c.write(20, 12, &[3]).unwrap();
        assert_eq!(c.port.requests[0], (12, 20, vec![3]));
    }

    #[test]
    fn codec_round_trips_each_type() {
        let cases = [
            (DataType::U8, 3, Value::Vector(vec![2.0, 1.0, 1.0])),
            (DataType::I32, 1, Value::Int(-4)),
            (DataType::U32, 1, Value::Int(0xFF8000)),
            (DataType::U16, 2, Value::Vector(vec![359.0, 1.0])),
            (DataType::F32, 1, Value::Float(0.5)),
            (DataType::Radians, 2, Value::Vector(vec![1.5, -0.25])),
            (DataType::Char, 6, Value::Text("2.1.1".into())),
        ];
        for (ty, count, value) in cases {
            let bytes = encode(ty, count, &value).unwrap();
            assert_eq!(bytes.len(), count * ty.size(), "{ty:?}");
            assert_eq!(decode(ty, count, &bytes), value, "{ty:?}");
        }
    }

    #[test]
    fn floats_are_little_endian() {
        assert_eq!(
            encode(DataType::F32, 1, &Value::Float(1.0)).unwrap(),
            [0x00, 0x00, 0x80, 0x3F]
        );
    }

    #[test]
    fn encode_rejects_values_that_do_not_fit() {
        assert!(encode(DataType::U8, 1, &Value::Int(256)).is_err());
        assert!(encode(DataType::U8, 2, &Value::Vector(vec![1.5, 0.0])).is_err());
        assert!(encode(DataType::I32, 2, &Value::Int(1)).is_err());
    }
}
