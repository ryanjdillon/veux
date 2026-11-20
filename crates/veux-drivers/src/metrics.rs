//! Live levels and test tones.
//!
//! A level meter reads a raw capture stream in 100 ms blocks and keeps the
//! latest peak and RMS of each channel. The stream starts with a thump when
//! the capture opens, so the first second does not count.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The level of one channel, in dBFS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub name: String,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    /// The highest peak of the last seconds, so short peaks stay visible.
    pub peak_hold_dbfs: f64,
}

/// What a device reports live. Each field is empty when the device cannot
/// report it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub levels: Vec<Level>,
    /// Direction of arrival of speech, in degrees.
    pub direction_deg: Option<f64>,
    pub speech: Option<bool>,
    /// A state to show instead of levels, for example "starting".
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToneChannel {
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tone {
    pub frequency_hz: u32,
    pub channel: ToneChannel,
}

/// The floor of the scale: silence and digital zero show as this.
pub const FLOOR_DBFS: f64 = -96.0;

pub fn dbfs(amplitude: f64) -> f64 {
    if amplitude <= 0.0 {
        return FLOOR_DBFS;
    }
    (20.0 * (amplitude / 32768.0).log10()).max(FLOOR_DBFS)
}

/// Peak and RMS in dBFS of each channel of interleaved 16-bit samples. The
/// DC offset is removed from the RMS.
pub fn levels(samples: &[i16], channels: usize) -> Vec<(f64, f64)> {
    (0..channels)
        .map(|ch| {
            let s: Vec<f64> = samples
                .iter()
                .skip(ch)
                .step_by(channels)
                .map(|&x| f64::from(x))
                .collect();
            if s.is_empty() {
                return (FLOOR_DBFS, FLOOR_DBFS);
            }
            let mean = s.iter().sum::<f64>() / s.len() as f64;
            let peak = s.iter().map(|x| x.abs()).fold(0.0, f64::max);
            let rms = (s.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
            (dbfs(peak), dbfs(rms))
        })
        .collect()
}

const HOLD: Duration = Duration::from_millis(1500);

#[derive(Default)]
struct State {
    levels: Vec<Level>,
    holds: Vec<(f64, Instant)>,
    blocks: u64,
    ended: Option<String>,
}

/// Reads a capture stream in a thread and keeps the latest levels.
pub struct LevelMeter {
    state: Arc<Mutex<State>>,
    _thread: JoinHandle<()>,
    warmup_blocks: u64,
}

impl LevelMeter {
    /// Start reading `stream`: raw S16_LE samples, `channels` interleaved, at
    /// `rate` Hz.
    pub fn start(
        stream: impl Read + Send + 'static,
        rate: u32,
        channels: usize,
        names: Vec<String>,
    ) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let frames = (rate / 10) as usize;
        let thread = std::thread::spawn(move || {
            let mut stream = stream;
            let mut buf = vec![0u8; frames * channels * 2];
            loop {
                if let Err(e) = stream.read_exact(&mut buf) {
                    shared.lock().expect("meter state").ended = Some(format!("capture ended: {e}"));
                    return;
                }
                let samples: Vec<i16> = buf
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b))
                    .collect();
                let now = Instant::now();
                let mut st = shared.lock().expect("meter state");
                let lv = levels(&samples, channels);
                if st.holds.len() != lv.len() {
                    st.holds = vec![(FLOOR_DBFS, now); lv.len()];
                }
                st.levels = lv
                    .iter()
                    .enumerate()
                    .map(|(i, &(peak, rms))| {
                        let (held, at) = st.holds[i];
                        let hold = if peak >= held || now.duration_since(at) > HOLD {
                            (peak, now)
                        } else {
                            (held, at)
                        };
                        st.holds[i] = hold;
                        Level {
                            name: names
                                .get(i)
                                .cloned()
                                .unwrap_or_else(|| format!("channel {i}")),
                            peak_dbfs: peak,
                            rms_dbfs: rms,
                            peak_hold_dbfs: hold.0,
                        }
                    })
                    .collect();
                st.blocks += 1;
            }
        });
        Self {
            state,
            _thread: thread,
            warmup_blocks: 10,
        }
    }

    /// The latest levels. The first second shows "starting", and a stream
    /// that ended shows why.
    pub fn read(&self) -> Metrics {
        let mut st = self.state.lock().expect("meter state");
        if let Some(why) = &st.ended {
            return Metrics {
                note: Some(why.clone()),
                ..Default::default()
            };
        }
        if st.blocks <= self.warmup_blocks {
            // The opening thump must not stay in the peak hold.
            for hold in &mut st.holds {
                hold.0 = FLOOR_DBFS;
            }
            return Metrics {
                note: Some("starting".into()),
                ..Default::default()
            };
        }
        Metrics {
            levels: st.levels.clone(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// 16 kHz stereo: a sine of `amp` on the left, silence on the right.
    fn sine_stereo(seconds: f64, amp: f64) -> Vec<u8> {
        let n = (16000.0 * seconds) as usize;
        let mut out = Vec::with_capacity(n * 4);
        for i in 0..n {
            let l = (amp * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16000.0).sin()) as i16;
            out.extend_from_slice(&l.to_le_bytes());
            out.extend_from_slice(&0i16.to_le_bytes());
        }
        out
    }

    #[test]
    fn levels_of_a_full_scale_sine() {
        let bytes = sine_stereo(0.1, 32767.0);
        let samples: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        let lv = levels(&samples, 2);
        assert!(lv[0].0 > -0.1, "peak {}", lv[0].0);
        // A sine's RMS is 3 dB below its peak.
        assert!((lv[0].1 + 3.01).abs() < 0.1, "rms {}", lv[0].1);
        assert_eq!(lv[1], (FLOOR_DBFS, FLOOR_DBFS));
    }

    #[test]
    fn meter_waits_one_second_then_reports() {
        let mut stream = sine_stereo(1.0, 32767.0); // the "thump"
        stream.extend(sine_stereo(0.5, 3276.7)); // -20 dBFS peak
        let meter = LevelMeter::start(Cursor::new(stream), 16000, 2, vec!["L".into(), "R".into()]);
        // The stream ends after 1.5 s of audio; wait for the thread.
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            if meter.state.lock().unwrap().ended.is_some() {
                break;
            }
        }
        let st = meter.state.lock().unwrap();
        assert_eq!(st.blocks, 15);
        let left = &st.levels[0];
        assert!(
            (left.peak_dbfs + 20.0).abs() < 0.2,
            "peak {}",
            left.peak_dbfs
        );
        drop(st);
        assert!(meter.read().note.unwrap().contains("capture ended"));
    }

    #[test]
    fn meter_shows_starting_during_warmup() {
        let meter = LevelMeter::start(Cursor::new(sine_stereo(0.3, 1000.0)), 16000, 2, vec![]);
        std::thread::sleep(Duration::from_millis(50));
        // Three blocks were read, the warm-up is ten, then the stream ended.
        let m = meter.read();
        assert!(m.levels.is_empty());
        assert!(m.note.is_some());
    }
}
