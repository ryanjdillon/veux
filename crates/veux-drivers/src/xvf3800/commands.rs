//! XVF3800 command table: facts from the sources in `docs/devices/xvf3800.md`.
//!
//! Parameter IDs are `<group>.<vendor name in lower case>`, so that the
//! vendor docs stay searchable. Commands that are not documented with a type
//! and an access mode are not in the table.

use veux_model::{Access, ParamSpec, ParamType};

use super::protocol::DataType::{self, Char, F32, I32, Radians, U8, U16, U32};

pub type Version = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Range {
    /// The full range of the data type.
    Type,
    Int(i64, i64),
    Float(f64, f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Command {
    pub id: &'static str,
    pub resource: u8,
    pub command: u8,
    pub ty: DataType,
    pub count: usize,
    pub access: Access,
    pub range: Range,
    pub unit: Option<&'static str>,
    /// The first firmware version that has the command.
    pub since: Version,
    pub description: &'static str,
}

impl Command {
    const fn new(
        id: &'static str,
        resource: u8,
        command: u8,
        ty: DataType,
        count: usize,
        access: Access,
        description: &'static str,
    ) -> Self {
        Self {
            id,
            resource,
            command,
            ty,
            count,
            access,
            range: Range::Type,
            unit: None,
            since: (0, 0, 0),
            description,
        }
    }

    const fn int(mut self, min: i64, max: i64) -> Self {
        self.range = Range::Int(min, max);
        self
    }

    const fn float(mut self, min: f64, max: f64) -> Self {
        self.range = Range::Float(min, max);
        self
    }

    const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = Some(unit);
        self
    }

    const fn since(mut self, major: u8, minor: u8, patch: u8) -> Self {
        self.since = (major, minor, patch);
        self
    }

    pub fn payload_len(&self) -> usize {
        self.count * self.ty.size()
    }

    pub fn to_spec(&self) -> ParamSpec {
        let group = self.id.split_once('.').map_or(self.id, |(g, _)| g);
        let (int_min, int_max) = match self.range {
            Range::Int(min, max) => (min, max),
            _ => type_int_range(self.ty),
        };
        let (float_min, float_max) = match self.range {
            Range::Float(min, max) => (min, max),
            Range::Int(min, max) => (min as f64, max as f64),
            Range::Type if is_float(self.ty) => (f32::MIN.into(), f32::MAX.into()),
            Range::Type => (int_min as f64, int_max as f64),
        };
        let ty = match (self.ty, self.count) {
            (Char, n) => ParamType::Text { max_len: n },
            (t, 1) if is_float(t) => ParamType::Float {
                min: float_min,
                max: float_max,
            },
            (_, 1) => ParamType::Int {
                min: int_min,
                max: int_max,
            },
            (_, n) => ParamType::Vector {
                len: n,
                min: float_min,
                max: float_max,
            },
        };
        ParamSpec {
            id: self.id.into(),
            group: group.into(),
            description: self.description.into(),
            ty,
            unit: self.unit.map(Into::into),
            access: self.access,
            // SAVE_CONFIGURATION stores all writable parameters.
            persistent: self.access == Access::ReadWrite,
        }
    }
}

fn is_float(ty: DataType) -> bool {
    matches!(ty, F32 | Radians)
}

fn type_int_range(ty: DataType) -> (i64, i64) {
    match ty {
        U8 | Char => (0, u8::MAX.into()),
        U16 => (0, u16::MAX.into()),
        I32 => (i32::MIN.into(), i32::MAX.into()),
        U32 => (0, u32::MAX.into()),
        F32 | Radians => (i64::MIN, i64::MAX),
    }
}

/// Write-only commands. They are actions, not parameters.
pub mod action {
    pub const REBOOT: (u8, u8) = (48, 7);
    pub const SAVE_CONFIGURATION: (u8, u8) = (48, 9);
    pub const CLEAR_CONFIGURATION: (u8, u8) = (48, 10);
}

pub const VERSION: Command = Command::new(
    "app.version",
    48,
    0,
    U8,
    3,
    RO,
    "Firmware version: major, minor, patch.",
);

const RO: Access = Access::ReadOnly;
const RW: Access = Access::ReadWrite;

#[rustfmt::skip]
pub static COMMANDS: &[Command] = &[
    // Application (resource 48).
    VERSION,
    Command::new("app.bld_msg", 48, 1, Char, 50, RO, "Build message."),
    Command::new("app.bld_host", 48, 2, Char, 30, RO, "Build host."),
    Command::new("app.bld_repo_hash", 48, 3, Char, 40, RO, "Build repository hash."),
    Command::new("app.bld_modified", 48, 4, Char, 6, RO, "Build from a modified tree."),
    Command::new("app.boot_status", 48, 5, Char, 3, RO, "Boot status."),
    Command::new("app.usb_bit_depth", 48, 8, U8, 2, RW, "USB bit depth: in, out (16, 24, or 32). A write reboots the device.").int(16, 32),
    Command::new("aic3104.hp_level", 48, 11, U8, 1, RW, "Headphone level.").int(0, 9).unit("dB").since(2, 1, 0),
    Command::new("aic3104.lineout_level", 48, 12, U8, 1, RW, "Line out level.").int(0, 9).unit("dB").since(2, 1, 0),

    // AEC and beamformer (resource 33).
    Command::new("aec.shf_bypass", 33, 70, U8, 1, RW, "Bypass the AEC.").int(0, 1),
    Command::new("aec.aecpathchange", 33, 0, I32, 1, RO, "AEC path change detected.").int(0, 1),
    Command::new("aec.hpfonoff", 33, 1, I32, 1, RW, "High-pass filter: 0 off, 1 70 Hz, 2 125 Hz, 3 150 Hz, 4 180 Hz.").int(0, 4),
    Command::new("aec.aecsilencelevel", 33, 2, F32, 2, RW, "Silence level: set, current.").float(0.0, 1.0),
    Command::new("aec.aecconverged", 33, 3, I32, 1, RO, "AEC converged.").int(0, 1),
    Command::new("aec.aecemphasisonoff", 33, 4, I32, 1, RW, "Emphasis: 0 off, 1 on, 2 on with EQ.").int(0, 2),
    Command::new("aec.far_extgain", 33, 5, F32, 1, RW, "External gain of the far-end signal.").unit("dB"),
    Command::new("aec.pcd_couplingi", 33, 6, F32, 1, RW, "Path change detection coupling.").float(0.0, 1.0),
    Command::new("aec.pcd_minthr", 33, 7, F32, 1, RW, "Path change detection minimum threshold.").float(0.0, 0.02),
    Command::new("aec.pcd_maxthr", 33, 8, F32, 1, RW, "Path change detection maximum threshold.").float(0.025, 0.2),
    Command::new("aec.rt60", 33, 9, F32, 1, RO, "Room reverberation time. Negative means not valid.").unit("s"),
    Command::new("aec.asroutonoff", 33, 35, I32, 1, RW, "ASR output: 0 AEC residuals, 1 ASR beams.").int(0, 1),
    Command::new("aec.asroutgain", 33, 36, F32, 1, RW, "Linear gain of the ASR output.").float(0.0, 1000.0),
    Command::new("aec.fixedbeamsonoff", 33, 37, I32, 1, RW, "Fixed beams.").int(0, 1),
    Command::new("aec.fixedbeamnoisethr", 33, 38, F32, 2, RW, "Fixed beam noise thresholds.").float(0.0, 1.0),
    Command::new("aec.num_mics", 33, 71, I32, 1, RO, "Number of microphones."),
    Command::new("aec.num_farends", 33, 72, I32, 1, RO, "Number of far-end channels."),
    Command::new("aec.mic_array_type", 33, 73, I32, 1, RO, "Array type: 1 linear, 2 squarecular."),
    Command::new("aec.mic_array_geo", 33, 74, F32, 12, RO, "Microphone positions: x, y, z for each microphone.").unit("m"),
    Command::new("aec.azimuth_values", 33, 75, Radians, 4, RO, "Beam directions: beam 1, beam 2, free-running, auto-select.").unit("rad"),
    Command::new("aec.spenergy_values", 33, 80, F32, 4, RO, "Speech energy of each beam. More than 0 means speech."),
    Command::new("aec.fixedbeamsazimuth_values", 33, 81, Radians, 2, RW, "Fixed beam azimuths.").unit("rad"),
    Command::new("aec.fixedbeamselevation_values", 33, 82, Radians, 2, RW, "Fixed beam elevations.").unit("rad"),
    Command::new("aec.fixedbeamsgating", 33, 83, U8, 1, RW, "Fixed beam gating."),

    // Audio manager (resource 35).
    Command::new("audio_mgr.mic_gain", 35, 0, F32, 1, RW, "Microphone gain before the AEC."),
    Command::new("audio_mgr.ref_gain", 35, 1, F32, 1, RW, "Reference gain."),
    Command::new("audio_mgr.selected_azimuths", 35, 11, Radians, 2, RO, "Processed direction (NaN without speech), auto-select direction.").unit("rad"),
    Command::new("audio_mgr.selected_channels", 35, 12, U8, 2, RW, "Selected channels."),
    Command::new("audio_mgr.op_l", 35, 15, U8, 2, RW, "Left output: category, source."),
    Command::new("audio_mgr.op_r", 35, 19, U8, 2, RW, "Right output: category, source."),
    Command::new("audio_mgr.op_all", 35, 23, U8, 12, RW, "All outputs: category, source for each channel."),
    Command::new("audio_mgr.sys_delay", 35, 26, I32, 1, RW, "System delay.").unit("samples"),
    Command::new("audio_mgr.op_ch3", 35, 28, U8, 2, RW, "Channel 3 output: category, source.").since(2, 1, 0),
    Command::new("audio_mgr.op_ch4", 35, 29, U8, 2, RW, "Channel 4 output: category, source.").since(2, 1, 0),
    Command::new("audio_mgr.op_ch5", 35, 30, U8, 2, RW, "Channel 5 output: category, source.").since(2, 1, 0),
    Command::new("audio_mgr.op_ch6", 35, 31, U8, 2, RW, "Channel 6 output: category, source.").since(2, 1, 0),

    // GPO and LEDs (resource 20).
    Command::new("gpo.read_values", 20, 0, U8, 5, RO, "Output pins: X0D11, X0D30 (mute), X0D31 (amp enable, active low), X0D33 (LED power), X0D39."),
    Command::new("led.effect", 20, 12, U8, 1, RW, "LED effect: 0 off, 1 breath, 2 rainbow, 3 single color, 4 DoA, 5 ring.").int(0, 5),
    Command::new("led.brightness", 20, 13, U8, 1, RW, "Brightness of the breath and rainbow effects."),
    Command::new("led.gammify", 20, 14, U8, 1, RW, "Gamma correction.").int(0, 1),
    Command::new("led.speed", 20, 15, U8, 1, RW, "Speed of the breath and rainbow effects."),
    Command::new("led.color", 20, 16, U32, 1, RW, "Color as 0xRRGGBB.").int(0, 0xFF_FFFF),
    Command::new("led.doa_color", 20, 17, U32, 2, RW, "DoA effect colors: base, direction.").int(0, 0xFF_FFFF),
    Command::new("led.ring_color", 20, 19, U32, 12, RW, "Color of each LED in ring mode.").int(0, 0xFF_FFFF).since(2, 0, 7),
    Command::new("doa.value", 20, 18, U16, 2, RO, "Direction of arrival in degrees, speech detected (0 or 1).").since(2, 0, 6),

    // GPI (resource 36).
    Command::new("gpi.read_values", 36, 0, U8, 3, RO, "Input pins: X1D09 (mute button), X1D13, X1D34."),
    Command::new("gpi.mute_function_enable", 36, 8, U8, 1, RW, "Mute button function.").int(0, 1).since(2, 1, 1),

    // Post-processing (resource 17).
    Command::new("pp.agconoff", 17, 10, I32, 1, RW, "Automatic gain control.").int(0, 1),
    Command::new("pp.agcmaxgain", 17, 11, F32, 1, RW, "AGC maximum gain.").float(1.0, 1000.0),
    Command::new("pp.agcdesiredlevel", 17, 12, F32, 1, RW, "AGC target level.").float(1e-8, 1.0),
    Command::new("pp.agcgain", 17, 13, F32, 1, RW, "AGC gain.").float(1.0, 1000.0),
    Command::new("pp.agctime", 17, 14, F32, 1, RW, "AGC time constant.").float(0.5, 4.0).unit("s"),
    Command::new("pp.agcfasttime", 17, 15, F32, 1, RW, "AGC fast time constant.").float(0.05, 4.0).unit("s"),
    Command::new("pp.agcalphafastgain", 17, 16, F32, 1, RW, "AGC fast gain threshold.").float(0.0, 1000.0),
    Command::new("pp.agcalphaslow", 17, 17, F32, 1, RW, "AGC slow smoothing.").float(0.0, 1.0),
    Command::new("pp.agcalphafast", 17, 18, F32, 1, RW, "AGC fast smoothing.").float(0.0, 1.0),
    Command::new("pp.limitonoff", 17, 19, I32, 1, RW, "Limiter.").int(0, 1),
    Command::new("pp.limitplimit", 17, 20, F32, 1, RW, "Limiter level.").float(1e-8, 1.0),
    Command::new("pp.min_ns", 17, 21, F32, 1, RW, "Stationary noise suppression floor.").float(0.0, 1.0),
    Command::new("pp.min_nn", 17, 22, F32, 1, RW, "Non-stationary noise suppression floor.").float(0.0, 1.0),
    Command::new("pp.echoonoff", 17, 23, I32, 1, RW, "Echo suppression.").int(0, 1),
    Command::new("pp.gamma_e", 17, 24, F32, 1, RW, "Echo over-subtraction.").float(0.0, 2.0),
    Command::new("pp.gamma_etail", 17, 25, F32, 1, RW, "Echo tail over-subtraction.").float(0.0, 2.0),
    Command::new("pp.gamma_enl", 17, 26, F32, 1, RW, "Non-linear echo over-subtraction.").float(0.0, 5.0),
    Command::new("pp.nlattenonoff", 17, 27, I32, 1, RW, "Non-linear echo attenuation.").int(0, 1),
    Command::new("pp.nlaec_mode", 17, 28, I32, 1, RW, "Non-linear AEC mode.").int(0, 2),
    Command::new("pp.mgscale", 17, 29, F32, 3, RW, "Gain scale: maximum, minimum, current."),
    Command::new("pp.fmin_speindex", 17, 30, F32, 1, RW, "Lowest frequency for speech detection.").float(0.0, 7999.0).unit("Hz"),
    Command::new("pp.dtsensitive", 17, 31, I32, 1, RW, "Double-talk sensitivity: 0 to 5, or 10 to 15.").int(0, 15),
    Command::new("pp.attns_mode", 17, 32, I32, 1, RW, "Noise attenuation mode.").int(0, 1),
    Command::new("pp.attns_nominal", 17, 33, F32, 1, RW, "Nominal noise attenuation.").float(0.0, 1.0),
    Command::new("pp.attns_slope", 17, 34, F32, 1, RW, "Noise attenuation slope.").float(0.0, 5.0),
];

pub fn find(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::protocol::MAX_TRANSFER;
    use super::*;

    #[test]
    fn ids_and_addresses_are_unique() {
        let mut ids = HashSet::new();
        let mut addresses = HashSet::new();
        for c in COMMANDS {
            assert!(ids.insert(c.id), "duplicate ID {}", c.id);
            assert!(
                addresses.insert((c.resource, c.command)),
                "duplicate address of {}",
                c.id
            );
            assert!(c.command < 0x80, "{} uses the read flag bit", c.id);
        }
    }

    #[test]
    fn every_read_fits_one_transfer() {
        for c in COMMANDS {
            assert!(c.payload_len() < MAX_TRANSFER, "{} is too large", c.id);
        }
    }

    #[test]
    fn spec_types_follow_count_and_range() {
        let spec = find("led.effect").unwrap().to_spec();
        assert_eq!(spec.ty, ParamType::Int { min: 0, max: 5 });
        assert_eq!(spec.group, "led");
        assert!(spec.persistent);

        let spec = find("aec.azimuth_values").unwrap().to_spec();
        assert!(matches!(spec.ty, ParamType::Vector { len: 4, .. }));
        assert!(!spec.persistent);

        let spec = find("app.bld_msg").unwrap().to_spec();
        assert_eq!(spec.ty, ParamType::Text { max_len: 50 });
    }
}
