//! The helper protocol: newline-delimited JSON over a byte stream.
//!
//! veux runs `veux helper` on a remote host, usually over SSH, and talks to
//! it on stdin and stdout. Each line is one [`Request`] or one [`Response`].
//! The first request must be [`Call::Hello`], so that both sides agree on
//! [`PROTOCOL_VERSION`]. A host can serve more than one device: [`Call::Open`]
//! returns a handle, and later calls name that handle.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::rc::Rc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use veux_drivers::metrics::{Metrics, Tone};
use veux_drivers::{Device, DeviceError, DeviceSpec, FirmwareInfo, Persisted};
use veux_model::{Schema, Values};

/// Change this when a message changes shape.
pub const PROTOCOL_VERSION: u32 = 7;

pub type Handle = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub call: Call,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    Hello {
        protocol: u32,
    },
    Open {
        spec: DeviceSpec,
    },
    Describe {
        handle: Handle,
    },
    Schema {
        handle: Handle,
    },
    ReadAll {
        handle: Handle,
    },
    Write {
        handle: Handle,
        values: Values,
    },
    Persist {
        handle: Handle,
    },
    ReadPersisted {
        handle: Handle,
    },
    Metrics {
        handle: Handle,
    },
    StopMetrics {
        handle: Handle,
    },
    CanPlayTone {
        handle: Handle,
    },
    PlayTone {
        handle: Handle,
        tone: Tone,
    },
    FirmwareInfo {
        spec: DeviceSpec,
    },
    /// `image` is the firmware file in base64.
    Flash {
        spec: DeviceSpec,
        image: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub result: Result<Reply, DeviceError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Hello {
        protocol: u32,
        version: String,
        arch: String,
    },
    Opened {
        handle: Handle,
    },
    Text(String),
    Schema(Schema),
    Values(Values),
    Persisted(Option<Persisted>),
    Metrics(Option<Metrics>),
    Firmware(FirmwareInfo),
    Bool(bool),
    Done,
}

/// What the helper serves: devices, and firmware updates. Firmware is not a
/// device operation, because a device in DFU mode cannot be opened.
pub trait Backend {
    fn open(&mut self, spec: &DeviceSpec) -> Result<Box<dyn Device>, DeviceError>;
    fn firmware_info(&mut self, spec: &DeviceSpec) -> Result<FirmwareInfo, DeviceError>;
    fn flash(&mut self, spec: &DeviceSpec, image: &[u8]) -> Result<(), DeviceError>;
}

/// The devices attached to this machine.
pub struct LocalBackend;

impl Backend for LocalBackend {
    fn open(&mut self, spec: &DeviceSpec) -> Result<Box<dyn Device>, DeviceError> {
        veux_drivers::open_local(spec)
    }

    fn firmware_info(&mut self, spec: &DeviceSpec) -> Result<FirmwareInfo, DeviceError> {
        veux_drivers::firmware_info(spec)
    }

    fn flash(&mut self, spec: &DeviceSpec, image: &[u8]) -> Result<(), DeviceError> {
        veux_drivers::flash_firmware(spec, image, &mut |_, _| {})
    }
}

/// Serve requests until the input ends.
pub fn serve(
    input: impl BufRead,
    mut output: impl Write,
    backend: &mut impl Backend,
) -> std::io::Result<()> {
    let mut devices: BTreeMap<Handle, Box<dyn Device>> = BTreeMap::new();
    let mut greeted = false;
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response {
                id: 0,
                result: Err(DeviceError(format!("bad request: {e}"))),
            },
            Ok(request) => Response {
                id: request.id,
                result: handle(request.call, &mut devices, &mut greeted, backend),
            },
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

fn handle(
    call: Call,
    devices: &mut BTreeMap<Handle, Box<dyn Device>>,
    greeted: &mut bool,
    backend: &mut impl Backend,
) -> Result<Reply, DeviceError> {
    if let Call::Hello { protocol } = call {
        if protocol != PROTOCOL_VERSION {
            return Err(DeviceError(format!(
                "protocol {protocol} is not supported: the helper speaks {PROTOCOL_VERSION}"
            )));
        }
        *greeted = true;
        return Ok(Reply::Hello {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
            arch: std::env::consts::ARCH.into(),
        });
    }
    if !*greeted {
        return Err(DeviceError("the first request must be hello".into()));
    }
    match call {
        Call::Hello { .. } => unreachable!("handled above"),
        Call::Open { spec } => {
            let opened = backend.open(&spec)?;
            let handle = devices.keys().next_back().map_or(1, |h| h + 1);
            devices.insert(handle, opened);
            Ok(Reply::Opened { handle })
        }
        Call::Describe { handle } => Ok(Reply::Text(device(devices, handle)?.describe())),
        Call::Schema { handle } => Ok(Reply::Schema(device(devices, handle)?.schema())),
        Call::ReadAll { handle } => Ok(Reply::Values(device(devices, handle)?.read_all()?)),
        Call::Write { handle, values } => {
            device(devices, handle)?.write(&values)?;
            Ok(Reply::Done)
        }
        Call::Persist { handle } => {
            device(devices, handle)?.persist()?;
            Ok(Reply::Done)
        }
        Call::ReadPersisted { handle } => {
            Ok(Reply::Persisted(device(devices, handle)?.read_persisted()?))
        }
        Call::Metrics { handle } => Ok(Reply::Metrics(device(devices, handle)?.metrics()?)),
        Call::StopMetrics { handle } => {
            device(devices, handle)?.stop_metrics();
            Ok(Reply::Done)
        }
        Call::CanPlayTone { handle } => Ok(Reply::Bool(device(devices, handle)?.can_play_tone())),
        Call::PlayTone { handle, tone } => {
            device(devices, handle)?.play_tone(tone)?;
            Ok(Reply::Done)
        }
        Call::FirmwareInfo { spec } => Ok(Reply::Firmware(backend.firmware_info(&spec)?)),
        Call::Flash { spec, image } => {
            let image = BASE64
                .decode(image)
                .map_err(|e| DeviceError(format!("bad firmware image encoding: {e}")))?;
            backend.flash(&spec, &image)?;
            Ok(Reply::Done)
        }
    }
}

fn device(
    devices: &mut BTreeMap<Handle, Box<dyn Device>>,
    handle: Handle,
) -> Result<&mut Box<dyn Device>, DeviceError> {
    devices
        .get_mut(&handle)
        .ok_or_else(|| DeviceError(format!("no open device with handle {handle}")))
}

/// The client end of one helper session.
pub struct Session<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
    /// Architecture and version that the helper reported.
    pub peer: (String, String),
}

impl<R: BufRead, W: Write> Session<R, W> {
    /// Connect and agree on the protocol version.
    pub fn start(reader: R, writer: W) -> Result<Self, DeviceError> {
        let mut session = Self {
            reader,
            writer,
            next_id: 1,
            peer: (String::new(), String::new()),
        };
        match session.call(Call::Hello {
            protocol: PROTOCOL_VERSION,
        })? {
            Reply::Hello { version, arch, .. } => session.peer = (arch, version),
            other => return Err(unexpected(&other)),
        }
        Ok(session)
    }

    pub fn call(&mut self, call: Call) -> Result<Reply, DeviceError> {
        let id = self.next_id;
        self.next_id += 1;
        let line =
            serde_json::to_string(&Request { id, call }).map_err(DeviceError::from_display)?;
        let io = |e: std::io::Error| DeviceError(format!("helper connection: {e}"));
        writeln!(self.writer, "{line}").map_err(io)?;
        self.writer.flush().map_err(io)?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply).map_err(io)? == 0 {
            return Err(DeviceError("the helper closed the connection".into()));
        }
        let response: Response = serde_json::from_str(&reply)
            .map_err(|e| DeviceError(format!("bad helper response: {e}")))?;
        if response.id != id {
            return Err(DeviceError(format!(
                "helper answered request {} for request {id}",
                response.id
            )));
        }
        response.result
    }
}

impl<R: BufRead, W: Write> Session<R, W> {
    pub fn firmware_info(&mut self, spec: DeviceSpec) -> Result<FirmwareInfo, DeviceError> {
        match self.call(Call::FirmwareInfo { spec })? {
            Reply::Firmware(info) => Ok(info),
            other => Err(unexpected(&other)),
        }
    }

    /// Flash an image on the host. This blocks until the device is done.
    pub fn flash(&mut self, spec: DeviceSpec, image: &[u8]) -> Result<(), DeviceError> {
        match self.call(Call::Flash {
            spec,
            image: BASE64.encode(image),
        })? {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }
}

fn unexpected(reply: &Reply) -> DeviceError {
    DeviceError(format!("unexpected helper reply: {reply:?}"))
}

/// A device behind a helper. Devices of one host share the session.
pub struct RemoteDevice<R, W> {
    session: Rc<RefCell<Session<R, W>>>,
    handle: Handle,
    description: String,
    schema: Schema,
}

impl<R: BufRead, W: Write> RemoteDevice<R, W> {
    pub fn open(
        session: Rc<RefCell<Session<R, W>>>,
        spec: DeviceSpec,
    ) -> Result<Self, DeviceError> {
        let mut s = session.borrow_mut();
        let handle = match s.call(Call::Open { spec })? {
            Reply::Opened { handle } => handle,
            other => return Err(unexpected(&other)),
        };
        let description = match s.call(Call::Describe { handle })? {
            Reply::Text(text) => format!("{text} via helper on {}", s.peer.0),
            other => return Err(unexpected(&other)),
        };
        let schema = match s.call(Call::Schema { handle })? {
            Reply::Schema(schema) => schema,
            other => return Err(unexpected(&other)),
        };
        drop(s);
        Ok(Self {
            session,
            handle,
            description,
            schema,
        })
    }
}

impl<R: BufRead, W: Write> Device for RemoteDevice<R, W> {
    fn describe(&self) -> String {
        self.description.clone()
    }

    fn schema(&self) -> Schema {
        self.schema.clone()
    }

    fn read_all(&mut self) -> Result<Values, DeviceError> {
        match self.session.borrow_mut().call(Call::ReadAll {
            handle: self.handle,
        })? {
            Reply::Values(values) => Ok(values),
            other => Err(unexpected(&other)),
        }
    }

    fn write(&mut self, values: &Values) -> Result<(), DeviceError> {
        match self.session.borrow_mut().call(Call::Write {
            handle: self.handle,
            values: values.clone(),
        })? {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    fn persist(&mut self) -> Result<(), DeviceError> {
        match self.session.borrow_mut().call(Call::Persist {
            handle: self.handle,
        })? {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    fn read_persisted(&mut self) -> Result<Option<Persisted>, DeviceError> {
        match self.session.borrow_mut().call(Call::ReadPersisted {
            handle: self.handle,
        })? {
            Reply::Persisted(persisted) => Ok(persisted),
            other => Err(unexpected(&other)),
        }
    }

    fn metrics(&mut self) -> Result<Option<Metrics>, DeviceError> {
        match self.session.borrow_mut().call(Call::Metrics {
            handle: self.handle,
        })? {
            Reply::Metrics(metrics) => Ok(metrics),
            other => Err(unexpected(&other)),
        }
    }

    fn stop_metrics(&mut self) {
        // The capture also stops when the helper exits, so an error here
        // only leaves it running until then.
        let _ = self.session.borrow_mut().call(Call::StopMetrics {
            handle: self.handle,
        });
    }

    /// False when the helper does not answer.
    fn can_play_tone(&self) -> bool {
        matches!(
            self.session.borrow_mut().call(Call::CanPlayTone {
                handle: self.handle,
            }),
            Ok(Reply::Bool(true))
        )
    }

    fn play_tone(&mut self, tone: Tone) -> Result<(), DeviceError> {
        match self.session.borrow_mut().call(Call::PlayTone {
            handle: self.handle,
            tone,
        })? {
            Reply::Done => Ok(()),
            other => Err(unexpected(&other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;
    use std::thread;

    use veux_model::{Access, Link, ParamSpec, ParamType, Value};

    use super::*;

    /// One integer parameter that keeps what is written.
    struct Counter {
        values: Values,
        persisted: bool,
    }

    impl Device for Counter {
        fn describe(&self) -> String {
            format!("counter, persisted {}", self.persisted)
        }

        fn schema(&self) -> Schema {
            Schema::new([ParamSpec {
                id: "c.n".into(),
                group: "c".into(),
                description: String::new(),
                ty: ParamType::Int { min: 0, max: 9 },
                unit: None,
                access: Access::ReadWrite,
                persistent: true,
            }])
        }

        fn read_all(&mut self) -> Result<Values, DeviceError> {
            Ok(self.values.clone())
        }

        fn write(&mut self, values: &Values) -> Result<(), DeviceError> {
            self.values.extend(values.clone());
            Ok(())
        }

        fn persist(&mut self) -> Result<(), DeviceError> {
            self.persisted = true;
            Ok(())
        }

        fn read_persisted(&mut self) -> Result<Option<Persisted>, DeviceError> {
            Ok(self.persisted.then(|| Persisted::read(self.values.clone())))
        }

        /// The level is the counter value, so a test can see a write.
        fn metrics(&mut self) -> Result<Option<Metrics>, DeviceError> {
            let Some(Value::Int(n)) = self.values.get(&"c.n".into()) else {
                return Ok(None);
            };
            Ok(Some(Metrics {
                speech: Some(*n > 5),
                ..Default::default()
            }))
        }

        fn can_play_tone(&self) -> bool {
            true
        }

        fn play_tone(&mut self, tone: Tone) -> Result<(), DeviceError> {
            self.values
                .insert("c.n".into(), Value::Int(i64::from(tone.frequency_hz)));
            Ok(())
        }
    }

    fn spec(kind: &str) -> DeviceSpec {
        DeviceSpec {
            kind: kind.into(),
            link: Link::Alsa,
            selector: BTreeMap::new(),
        }
    }

    type Client = Session<BufReader<UnixStream>, UnixStream>;

    /// Opens `counter` devices and records flashed images.
    #[derive(Default)]
    struct FakeBackend {
        flashed: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    }

    impl Backend for FakeBackend {
        fn open(&mut self, spec: &DeviceSpec) -> Result<Box<dyn Device>, DeviceError> {
            match spec.kind.as_str() {
                "counter" => Ok(Box::new(Counter {
                    values: Values::from([("c.n".into(), Value::Int(1))]),
                    persisted: false,
                })),
                other => Err(DeviceError(format!("no device of kind {other}"))),
            }
        }

        fn firmware_info(&mut self, _: &DeviceSpec) -> Result<FirmwareInfo, DeviceError> {
            Ok(FirmwareInfo {
                version: Some("2.1.1".into()),
                slots: vec!["0 Factory".into(), "1 Upgrade".into()],
                dfu_mode: false,
                bundled_image: false,
            })
        }

        fn flash(&mut self, _: &DeviceSpec, image: &[u8]) -> Result<(), DeviceError> {
            self.flashed.lock().unwrap().push(image.to_vec());
            Ok(())
        }
    }

    /// A helper in a thread, connected over a socket pair.
    fn helper_with(backend: FakeBackend) -> (Client, thread::JoinHandle<()>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut backend = backend;
            let reader = BufReader::new(theirs.try_clone().unwrap());
            serve(reader, theirs, &mut backend).unwrap();
        });
        let session = Session::start(BufReader::new(ours.try_clone().unwrap()), ours).unwrap();
        (session, server)
    }

    fn helper() -> (Client, thread::JoinHandle<()>) {
        helper_with(FakeBackend::default())
    }

    #[test]
    fn firmware_info_and_flash_cross_the_session() {
        let backend = FakeBackend::default();
        let flashed = backend.flashed.clone();
        let (mut session, server) = helper_with(backend);
        let info = session.firmware_info(spec("xvf")).unwrap();
        assert_eq!(info.version.as_deref(), Some("2.1.1"));
        let image: Vec<u8> = (0..=255).collect();
        session.flash(spec("xvf"), &image).unwrap();
        drop(session);
        server.join().unwrap();
        assert_eq!(*flashed.lock().unwrap(), [image]);
    }

    #[test]
    fn remote_device_reads_writes_and_persists() {
        let (session, server) = helper();
        assert_eq!(session.peer.0, std::env::consts::ARCH);
        let session = Rc::new(RefCell::new(session));
        let mut device = RemoteDevice::open(session.clone(), spec("counter")).unwrap();
        assert!(device.schema().get(&"c.n".into()).is_some());
        assert_eq!(device.read_all().unwrap()[&"c.n".into()], Value::Int(1));
        device
            .write(&Values::from([("c.n".into(), Value::Int(7))]))
            .unwrap();
        assert_eq!(device.read_all().unwrap()[&"c.n".into()], Value::Int(7));
        assert_eq!(device.read_persisted().unwrap(), None);
        device.persist().unwrap();
        assert_eq!(
            device.read_persisted().unwrap().unwrap().values[&"c.n".into()],
            Value::Int(7)
        );
        drop(device);
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn metrics_and_tones_cross_the_session() {
        use veux_drivers::metrics::ToneChannel;
        let (session, server) = helper();
        let session = Rc::new(RefCell::new(session));
        let mut device = RemoteDevice::open(session.clone(), spec("counter")).unwrap();
        assert_eq!(device.metrics().unwrap().unwrap().speech, Some(false));
        assert!(device.can_play_tone());
        device
            .play_tone(Tone {
                frequency_hz: 9,
                channel: ToneChannel::Left,
            })
            .unwrap();
        assert_eq!(device.metrics().unwrap().unwrap().speech, Some(true));
        device.stop_metrics();
        drop(device);
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn open_error_reaches_the_client() {
        let (session, server) = helper();
        let session = Rc::new(RefCell::new(session));
        let error = RemoteDevice::open(session.clone(), spec("toaster"))
            .err()
            .unwrap();
        assert_eq!(error.0, "no device of kind toaster");
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn requests_before_hello_and_wrong_protocol_are_refused() {
        let mut devices = BTreeMap::new();
        let mut greeted = false;
        let mut backend = FakeBackend::default();
        let early = handle(
            Call::ReadAll { handle: 1 },
            &mut devices,
            &mut greeted,
            &mut backend,
        );
        assert_eq!(early.unwrap_err().0, "the first request must be hello");
        let wrong = handle(
            Call::Hello { protocol: 99 },
            &mut devices,
            &mut greeted,
            &mut backend,
        );
        assert!(
            wrong
                .unwrap_err()
                .0
                .contains("protocol 99 is not supported")
        );
        assert!(!greeted);
    }

    #[test]
    fn request_lines_are_readable_json() {
        let line = serde_json::to_string(&Request {
            id: 3,
            call: Call::Persist { handle: 1 },
        })
        .unwrap();
        assert_eq!(
            line,
            r#"{"id":3,"call":{"method":"persist","params":{"handle":1}}}"#
        );
    }
}
