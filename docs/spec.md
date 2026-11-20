# veux specification

Status: draft, 2026-11-09.

## Objective

veux is a terminal UI that configures ReSpeaker audio devices across a home
network. It reads, edits, monitors, and persists device settings from one
place. Desired state is kept in a declarative file under version control.

## Target devices

| Kind ID              | Hardware                              | Control surface                         |
|----------------------|---------------------------------------|-----------------------------------------|
| `xvf3800`            | ReSpeaker XVF3800 4-mic array         | USB vendor control, or ESPHome on XIAO  |
| `respeaker-2mic-v1`  | ReSpeaker 2-Mics Pi HAT v1.x          | ALSA mixer (WM8960), APA102 LEDs        |

The design must accept more ReSpeaker kinds without changes to the core.
A new kind is one new driver module.

## Settled decisions

| Topic            | Decision                                                                 |
|------------------|--------------------------------------------------------------------------|
| Language         | Rust, with `ratatui` for the TUI.                                        |
| Remote transport | SSH. No daemon on the devices.                                           |
| Remote code      | Self-push helper. An optional pre-installed helper path overrides it.    |
| XVF3800 links    | Local USB, remote USB over SSH, and ESPHome native API.                 |
| ESPHome depth    | Exposed entities first. Then a custom component with raw parameter RPC.  |
| Persistence      | Apply writes runtime values. A separate commit writes persistent state. |
| Pi scope (v1)    | Device audio configuration only.                                         |
| v1 features      | Live edit, declarative profiles, live monitoring, LEDs, firmware update. |

Deferred, but the driver interface must allow them later:

- Voice pipeline configuration (wyoming-satellite, linux-voice-assistant).
- Driver and overlay checks for the HAT codec.
- Control of systemd services.

## Architecture

### Concepts

- **Host**: a machine that veux reaches. It is `local` or an SSH target.
- **Link**: how veux reaches one device: `usb`, `alsa`, or `esphome`.
- **Driver**: code for one device kind. It declares a parameter schema and
  implements read, write, persist, monitor, LED, and firmware operations.
- **Parameter**: one typed, named setting, for example `aec.enabled` or
  `codec.adc_gain_db`.
- **Profile**: a named set of parameter values.

A driver does not know about SSH. A transport does not know about parameters.

### Process model

```text
  veux (TUI / CLI)  ──local──►  driver ──► libusb / ALSA
        │
        ├──SSH stdio──► veux helper (on remote host) ──► driver ──► libusb / ALSA
        │
        └──TCP 6053 (Noise)──► ESPHome device (XIAO ESP32S3 + XVF3800)
```

The helper is the same binary, started as `veux helper`. It speaks
newline-delimited JSON-RPC on stdin and stdout. One SSH session carries all
requests and monitor streams for a host.

### Self-push helper

1. veux runs `uname -m` on the host and selects the matching build
   (`aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`).
2. veux checks `~/.cache/veux/veux-<first 16 hex digits of the sha256>`. If
   the file is missing, veux uploads it to a `.part` name, then renames it.
3. veux runs the helper and checks the protocol version in the handshake.
4. If the host config sets `helper_path`, veux uses that path and does not
   upload. Use this on NixOS hosts that pin veux.

`just release` cross-builds the helpers with `cargo zigbuild` and embeds
them in the binary. `just build` embeds none; then `VEUX_HELPER_<ARCH>` can
name a helper file for development. The static helpers build libusb without
udev, because a musl binary cannot link the host libudev; libusb then uses
netlink.

The helper protocol is one JSON object per line. A request is
`{"id": n, "call": {"method": ..., "params": ...}}`. The methods are `hello`,
`open` (returns a handle), `describe`, `schema`, `read_all`, `write`,
`persist`, `read_persisted`, `metrics`, `stop_metrics`, `can_play_tone`,
`play_tone`, `firmware_info`, and `flash` (the image in base64). The
protocol version is 6. A response is `{"id": n, "result": {"Ok": ...}}` or
`{"id": n, "result": {"Err": "message"}}`.

### Crate layout

| Crate             | Responsibility                                                  |
|-------------------|-----------------------------------------------------------------|
| `veux-model`      | Parameter schema, values, profiles, three-state diff.           |
| `veux-drivers`    | `Device` trait, one module for each device kind, ESPHome client. |
| `veux-rpc`        | Helper protocol: messages, server loop, client `RemoteDevice`.  |
| `veux`            | Binary: TUI, CLI, `helper` mode, and the SSH transport.         |

### Driver interface

`veux-drivers` defines one trait for an open device. The CLI, the TUI, and
the helper all use it:

```rust
trait Device {
    fn describe(&self) -> String;
    fn schema(&self) -> Schema;          // what this device supports
    fn read_all(&mut self) -> Result<Values, DeviceError>;
    fn write(&mut self, values: &Values) -> Result<(), DeviceError>;
    fn persist(&mut self) -> Result<(), DeviceError>;
    // With defaults, for devices that cannot do these:
    fn read_persisted(&mut self) -> Result<Option<Persisted>, DeviceError>;
    fn metrics(&mut self) -> Result<Option<Metrics>, DeviceError>;
    fn stop_metrics(&mut self);
    fn can_play_tone(&self) -> bool;
    fn play_tone(&mut self, tone: Tone) -> Result<(), DeviceError>;
}
```

`open_local(&DeviceSpec)` opens a device that is attached to the machine
that runs it. `DeviceError` is a message, so that it can cross the helper
protocol. Firmware is not in the trait, because a device in DFU mode cannot
be opened: see `firmware_info` and `flash_firmware`.

Each schema entry has: ID, group, type (bool, int, float, enum, vector),
range, unit, access (read-only or read-write), and a flag for persistence.
The TUI renders its forms from the schema. It has no code for a specific
device kind.

The XVF3800 has two links with different schemas:

- `usb`: vendor control transfers through `rusb`. The schema is the XVF3800
  command table.
- `esphome`: the schema comes from the entities that the firmware exposes.
  Parameter IDs are `<platform>.<slug of the entity name>`. See
  `docs/devices/esphome.md`. When the firmware config has the two API
  actions `xvf_get` and `xvf_set`, the link also has the XVF3800 command
  table. Then the schema is the entities and the raw XVF3800 parameters,
  without those that the ESPHome component owns.

The 2-Mic HAT driver reads its controls from the card. See
`docs/devices/respeaker-2mic.md`.

## Configuration file

`veux.toml` holds the inventory and the profiles. Secrets are not in the
file. The file names environment variables that hold them.

A profile key is a parameter ID. Quoted IDs (`"aec.enabled" = true`), dotted
keys (`aec.enabled = true`), and sub-tables (`[profiles.p.aec]`) give the
same ID. `veux check` validates the file against the driver schemas.

```toml
[hosts.office-pi]
ssh = "pi@office-pi.local"
# helper_path = "/run/current-system/sw/bin/veux"

[hosts.kitchen-pi]
ssh = "pi@kitchen-pi.local"

[devices.office-array]
kind = "xvf3800"
link = "usb"
host = "office-pi"
match = { serial = "..." }
profile = "array-default"

[devices.living-array]
kind = "xvf3800"
link = "esphome"
address = "living-sat.local:6053"
noise_key_env = "VEUX_LIVING_NOISE_KEY"
profile = "array-default"

[devices.kitchen-hat]
kind = "respeaker-2mic-v1"
link = "alsa"
host = "kitchen-pi"
profile = "hat-default"

[profiles.array-default]
"aec.enabled" = true
"agc.max_gain_db" = 30

[profiles.hat-default]
"codec.adc_gain_db" = 12
```

## State model

Each parameter has three values:

| State     | Source                                                        |
|-----------|---------------------------------------------------------------|
| profile   | `veux.toml`                                                   |
| runtime   | The live device                                               |
| persisted | `/var/lib/alsa/asound.state` on the Pi (2-Mic HAT), or the snapshot of the last commit (XVF3800 on USB) |

veux shows drift when any two of the three values differ. A state that
veux cannot read is unknown, not different. Thus a device that is not
connected shows no drift.

For the 2-Mic HAT, the helper reads the card's block in `asound.state`,
which alsa-restore loads at boot. The XVF3800 cannot read back its flash.
Thus `commit` on the USB link records the runtime values in a snapshot,
and the snapshot is the persisted state. `Persisted.snapshot` marks it,
and the TUI and `plan` show that it is a snapshot. See `docs/devices/xvf3800.md`.
An ESPHome device saves its values by itself and cannot report them, so
`commit` does nothing there.

| Command                         | Effect                                                 |
|---------------------------------|--------------------------------------------------------|
| `veux`                          | Open the TUI.                                          |
| `veux plan [device...]`         | Show apply drift and commit drift. No names: all.      |
| `veux apply [device...] [--yes]`| Write profile values to runtime, after a confirmation. |
| `veux commit [device...] [--yes]` | Persist runtime values on the device.                |
| `veux export <device>`          | Print runtime values as a `[profiles.<name>]` table.   |
| `veux probe`                    | List local XVF3800 devices with version and values.    |
| `veux read <device> [id...]`    | Print runtime values.                                  |
| `veux write <device> id=value...` | Write runtime values (TOML syntax).                  |
| `veux firmware <device> [image \| --bundled]` | Show the firmware, or flash an image (asks first). `--bundled`: an ESPHome device writes the image of its build. |
| `veux meter <device> [--seconds N]` | Print live levels, direction, and speech.          |
| `veux tone <device> [--channel left\|right\|both] [--frequency HZ]` | Play a sine test tone. |
| `veux helper`                   | Internal. Run the JSON-RPC helper on stdio.            |

Live edits in the TUI write runtime values only. They never write flash.
In the TUI, `a` applies the profile and `c` commits, both after a
confirmation, and `u` sets the highlighted parameter back to its profile
value.

`export` writes TOML only. Nix does not own mixer values (see the ALSA
persistence decision), so there is no Nix or alsactl export.

## TUI layout

- Left pane: the device tree, grouped by host. Each device shows its
  connection state and its drift count.
- Main pane: the parameter table of the selected device, with a detail
  pane below it. The monitor and the firmware view replace the detail
  pane, so the table stays in view. LEDs are parameters in the table.
- Parameters tab: a table of groups with profile, runtime, and persisted
  columns, and a detail pane for the highlighted parameter. Edit in place.
  `s` saves a runtime value into the profile in `veux.toml` (with
  `toml_edit`, so comments and layout stay), `S` saves all edits of the
  session, and quit asks first when edits are unsaved. `a` asks to apply the profile of the
  device, and `c` asks to commit it.
- Monitor: `m` replaces the detail pane with live level meters, direction,
  and speech state of the selected device. `t` plays a test tone on a
  channel that you choose.
- Firmware: `f` replaces the detail pane with the running version, the DFU
  slots, and DFU-only mode. `F` asks for an image path, then shows its size
  and sha256 and asks again. On an ESPHome device with `xvf_flash`, the
  pane shows that the image comes from the ESPHome build, and `F` only
  asks once. The flash runs in a thread with progress. veux closes the
  device connection first, and keys wait until the flash ends.
- Status bar: the pending action, the last error, and key hints.
- Theme: values are coloured by type (numbers, booleans, options, text), and
  read-only rows are grey. `ligne-claire` (the default) paints its own cream
  background, so it reads the same on dark and light terminals; `plain`
  uses the terminal palette.

## Monitoring

`Device::metrics` returns the latest levels, direction, and speech state.
The first call starts the measurement and `stop_metrics` ends it. The TUI
reads at 10 Hz.

- XVF3800 over USB: direction and speech from `doa.value` (firmware 2.0.6
  and later), else from the processed beam azimuth, which is NaN without
  speech. No levels: the USB control interface has no level command.
- XVF3800 over ESPHome: none. The firmware exposes no live entities yet.
- 2-Mic HAT: the helper runs `arecord` on the card at 16 kHz and computes
  the peak and RMS of each microphone in 100 ms blocks, with a peak hold of
  1.5 s. The first second is ignored, because the capture starts with a
  thump. The test tone is `speaker-test`. See `docs/devices/respeaker-2mic.md`.

`Device::can_play_tone` tells which devices play a test tone. The TUI asks
it before the channel prompt.

- 2-Mic HAT: `speaker-test` on the card.
- ESPHome with a media player: veux serves a 3 s stereo FLAC on TCP port
  8790 and sends its URL to the media player. See `docs/devices/esphome.md`.
- XVF3800 over USB: not yet supported.

## LEDs

- XVF3800: LED ring commands (effect, color, brightness).
- XVF3800 over ESPHome: the LED ring is a light entity (`light.led_ring.*`).
- 2-Mic HAT: 3 APA102 LEDs over SPI0 (`/dev/spidev0.0`), as `led.power`,
  `led.brightness`, and `led.rgb`. The LEDs cannot be read back; the helper
  keeps the last written state on the host. See `docs/devices/respeaker-2mic.md`.

## Firmware

- XVF3800 over USB: USB DFU. veux checks the image version before the
  write and asks for confirmation.
- XVF3800 over ESPHome: only with the `xvf_get`, `xvf_set`, and `xvf_flash` actions. veux reads the version
  with `xvf_get`. The image comes with the ESPHome build, and veux runs the
  `xvf_flash` action and follows its progress entities. The entity-only
  mode does not support firmware updates. See `docs/devices/esphome.md`.
- 2-Mic HAT: no firmware. The driver returns `None`.

## Open questions

Verify each item on real hardware before you rely on it.

1. **XVF3800 command table.** Resolved. See `docs/devices/xvf3800.md`. The byte
   order and some ranges still need a hardware test.
2. **Persisted XVF3800 values.** Resolved. Flash values cannot be read
   back. veux records a snapshot at each commit and compares to it.
3. **2-Mic HAT mixer controls.** Partly resolved. The HAT v1.x has a
   WM8960 codec. `crates/veux-drivers/tests/fixtures/wm8960_asound.state`
   is an `alsactl` dump from the seeed-voicecard driver, with all 64
   controls. Confirm the card name and controls under the driver of the
   new host image.
4. **`asound.state` path and persistence.** Resolved for the NixOS
   satellite hosts. They set `hardware.alsa.enablePersistence = true`, so
   `alsa-restore` loads `/var/lib/alsa/asound.state` at boot and stores it
   at shutdown. veux owns the mixer values. The host config does not set
   them. Thus `commit` runs `alsactl store`.
5. **Capture contention.** The voice satellite can hold the capture device.
   The meter opens `hw:` directly and fails with a clear error when the
   device is busy. Metering beside the satellite needs `dsnoop`.
6. **ESPHome Rust client.** Check the available crates for the native API
   with Noise encryption. If none is good, implement the protocol subset.
7. **Existing ESPHome XVF3800 component.** Resolved. It is the
   `respeaker_xvf3800` component of formatBCE Respeaker-ESPHome-integration.
   It uses I2C, exposes few parameters, and supports DFU. See `docs/devices/xvf3800.md`.
8. **USB permissions.** Write a udev rule for the XVF3800 vendor and product
   IDs. Run the helper without root.
