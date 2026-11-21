# ESPHome link

This doc tells how veux reads and writes an ESPHome device, for example the
XVF3800 voice satellite on a XIAO ESP32S3.

## Requirements

| Need | Detail |
|------|--------|
| API encryption key | The key sits in the environment variable that `noise_key_env` names. See [Encryption key](#encryption-key). |
| Satellite firmware | formatBCE [Respeaker-ESPHome-integration](https://github.com/formatBCE/Respeaker-ESPHome-integration). Tested with release 2026.11.2, ESPHome 2026.11.0, and XVF3800 firmware 1.0.9. The earlier repository, Respeaker-XVF3800-ESPHome-integration, is frozen. veux was tested with it at ref `542a05d44b6eb2c646a6ba0acd3e0cf2e6edcfab` and ESPHome 2026.9. |
| Raw parameters, firmware view, bundled flash | The three API actions of [xvf-actions.yaml](../../examples/esphome/xvf-actions.yaml): `xvf_get`, `xvf_set`, and `xvf_flash`. Without them the link stays in entity mode. |
| Test tone | A media player that decodes FLAC, and inbound TCP port 8790 open on the machine that runs veux. See [Test tone](#test-tone). |

## Protocol

veux uses the ESPHome native API on TCP port 6053. The `esphome-client`
crate does the framing and the Noise handshake
(`Noise_NNpsk0_25519_ChaChaPoly_SHA256`). It is pinned to `=0.2.1` with the
`api-1-16` feature, which agrees with ESPHome 2026.9. ESPHome 2026.11 reports API 1.19.
The client works with it: the 2026-11-20 test used 2026.11.0.

- Since ESPHome 2026.1 there is no password. The encryption key is the only
  credential.
- On connect, veux lists the entities and the API actions, and subscribes
  to the states. The device then pushes each change, so a read only
  collects new messages.
- When the device has the `xvf_get` action, veux also reads the XVF3800
  firmware version and shows it in the device description. See "Raw
  parameter actions".
- Source of the message definitions:
  <https://github.com/esphome/esphome/blob/dev/esphome/components/api/api.proto>.

## Parameters

The schema comes from the device, not from the driver. Thus `veux check`
cannot check the values of a profile for an ESPHome device. veux checks
them when it connects.

A parameter ID is `<platform>.<slug>`. veux makes the slug from the entity
name: lower case letters and digits, and one `_` for each run of other
characters. Home Assistant adds the device name, so its entity ID differs:
`switch.wake_sound` in veux is `switch.living_room_satellite_wake_sound` in Home
Assistant. ESPHome 2026 sends
an empty `object_id`, so veux does not use it. If two entities of one
platform have the same name, the second gets `_2`. Run `veux read <device>`
to see the IDs.

When veux connects, the TUI reports each profile entry that the device does
not accept, for example an ID that is not on the device.

A device with the raw actions `xvf_get` and `xvf_set` also has the raw
XVF3800 parameters, with the USB IDs, for example `pp.gamma_e`. See "Raw
parameter actions".

| Platform        | veux type          | Access     |
|-----------------|--------------------|------------|
| `number`        | float, min to max  | read-write |
| `select`        | enum of options    | read-write |
| `switch`        | bool               | read-write |
| `sensor`        | float              | read-only  |
| `binary_sensor` | bool               | read-only  |
| `text_sensor`   | text               | read-only  |
| `light`         | see below          | read-write |
| `media_player`  | see below          | read-write |

A light gives one parameter for each value that its color modes support,
with the ID `light.<slug>.<field>`:

| Field        | veux type                   | When                          |
|--------------|-----------------------------|-------------------------------|
| `on`         | bool                        | always                        |
| `brightness` | float, 0 to 1               | a color mode has brightness   |
| `rgb`        | vector of 3 floats, 0 to 1  | a color mode has RGB          |
| `effect`     | enum of effects             | the light lists effects       |

A write sends a light command that changes only that field. A brightness or
color write does not turn the light on. The XVF3800 firmware exposes its LED
ring as `light.led_ring`.

A media player gives `media_player.<slug>.volume` (0 to 1) and
`media_player.<slug>.muted`. A volume write sends only the volume; a mute
write sends the mute or unmute command. Play, pause, and media are actions,
not settings, so they are not parameters. The firmware can limit the range
with `volume_min` and `volume_max`; the output codec gain is also in the
firmware config.

Buttons and API actions are not parameters yet.

## Speaker level on the XVF3800

Two parameters set the speaker level of the XVF3800 satellite:

| Parameter                          | Range        | What it sets                     |
|------------------------------------|--------------|----------------------------------|
| `media_player.media_player.volume` | 0 to 1       | Codec DAC level; 1 is 0 dB       |
| `number.output_gain`               | 0 to +9 dB   | Codec output stage, step 1 dB    |

- Set the volume first. If 1 is not loud enough, raise `number.output_gain`
  in steps of 1 dB while audio plays. A high gain with the volume near 1
  clips.
- `number.output_gain` ("Output gain", entity category config) is not in
  the formatBCE packages. A satellite config can add it as its own entity,
  and veux shows it when the device lists it. Part 4 of
  [xvf-actions.yaml](../../examples/esphome/xvf-actions.yaml) has the
  entity, its script, and the boot and interval triggers. In the tested config, the
  device keeps it across reboots. The firmware applies it 5 s after boot
  and again every 60 s, because the XVF3800 resets the codec on its own
  reboot.

## Test tone

`t` in the TUI and `veux tone` play a tone on the first media player of
the device.

1. veux makes a 3 s stereo FLAC at 48 kHz and -18 dBFS. One channel is
   silent for a left or right tone. The frames are not compressed
   (VERBATIM subframes).
2. veux listens on TCP port 8790 and sends the file URL in a
   `MediaPlayerCommandRequest`. The URL has the local address on the route
   to the device.
3. The device fetches the file one time. veux stops listening after the
   fetch, or after 10 s.

FLAC, not WAV: an ESPHome media player decodes only the formats that its
config asks for. The formatBCE core package (`packages/core/media-player.yaml`) asks for
FLAC, so a WAV URL fails as soon as the device reads the headers. Its
media pipeline is stereo, so left and right
work. The level follows the media player volume and the output gain.

The device must reach the machine that runs veux on port 8790. The firewall
of that machine must allow inbound TCP 8790 from the device. On NixOS:

```nix
networking.firewall.allowedTCPPorts = [ 8790 ];
```

On other systems, add an equivalent rule. For example, with nftables, accept
`tcp dport 8790` in the input chain. With iptables, run
`iptables -A INPUT -p tcp --dport 8790 -s <device-address> -j ACCEPT`.

## Persistence

ESPHome saves an entity with `restore_value` to flash by itself, after the
flash write interval (1 minute by default). veux cannot see which entities
do that, or read the saved value. Thus the persisted state of an ESPHome
parameter is unknown, and `commit` does nothing on this link.

The XVF3800 does not save these values. The ESP32 sends them to the XVF3800
again after each boot.

## Encryption key

The key is the `api: encryption: key:` value of the ESPHome config of the
device. In `veux.toml`, the device entry names an environment variable that
holds this base64 key (`noise_key_env`). Set the variable before you start
veux. veux never reads a secret file. For example:

```bash
export VEUX_API_KEY='<the encryption key of the device>'
```

## Raw parameter actions

The XVF3800 satellite firmware is the formatBCE
[Respeaker-ESPHome-integration](https://github.com/formatBCE/Respeaker-ESPHome-integration)
project. It has one core for all devices and one release. The XVF3800
packages are in `packages/xvf3800/`, and the component
`respeaker_xvf3800` is in `esphome/components/respeaker_xvf3800/`. The
firmware moved from the earlier repository
Respeaker-XVF3800-ESPHome-integration, which is frozen: old releases still
build, but there are no new ones. It does not expose the raw
XVF3800 parameters or a DFU update through the API. veux needs three extra
API actions for the raw parameters, the firmware view, and the bundled
flash. They give the ESPHome link the full XVF3800 command table of the USB
link.

Add the actions to the satellite config. The file
[`examples/esphome/xvf-actions.yaml`](../../examples/esphome/xvf-actions.yaml)
has the YAML, with comments that say where each part goes. It was tested
with release 2026.11.2 and ESPHome 2026.11.0, and earlier with the earlier
repository at ref `542a05d44b6eb2c646a6ba0acd3e0cf2e6edcfab` and ESPHome
2026.9. Merged into `config/respeaker-xvf-satellite-factory.yaml` of
release 2026.11.2, the file passes `esphome config` on ESPHome 2026.11.0.
A flash from a URL and an upstream change to formatBCE are not done.

Status:

- The config has `xvf_get`, `xvf_set`, and `xvf_flash` as API actions with
  responses. They are YAML only, because the formatBCE component
  (`id: respeaker`) is an I2C device with public `read` and `write`.
- `EsphomeDevice` is a `ControlPort`, so the USB driver `Xvf3800` runs over
  these actions.
- When a device lists `xvf_get`, veux reads the XVF3800 version at connect
  and shows it in the device description.
- The schema, reads, and writes have the raw parameters. See "Discovery".
- The firmware view and `veux firmware` use `xvf_get` and `xvf_flash`. See
  "Firmware".

Tested on a test satellite on 2026-11-20 with release 2026.11.2, ESPHome
2026.11.0, and XVF3800 firmware 1.0.9:

- The OTA update to the release worked. The XVF3800 updated itself from
  1.0.7 to 1.0.9 on boot.
- veux read all 85 raw values.
- The write round trip passed for `pp.dtsensitive`, `pp.gamma_e`, and
  `aec.fixedbeamnoisethr`.
- The bundled flash ran from 0 to 100 % and the device kept its settings.
- The left-channel test tone played.
- No entity was removed or renamed. The release adds
  `binary_sensor.snooze_pending`, `number.snooze_duration`,
  `switch.snooze_enabled`, and `text_sensor.dsp_status`.
- `aec.hpfonoff` reads 0 on firmware 1.0.9. It read 2 on 1.0.7. This is
  not checked against the vendor notes.

Earlier test, with the earlier repository (ref `542a05d`, ESPHome 2026.9),
on a test satellite on 2026-11-17: veux reads XVF3800 firmware
1.0.7. The ignored test `raw_actions_round_trip_on_a_device` checks the
hidden IDs, reads 79 values, and writes, reads back, and restores
`pp.dtsensitive`, `pp.gamma_e`, and `aec.fixedbeamnoisethr` through the
`Device` interface. All commands of the table for firmware 1.0.7 read
without an error on the I2S firmware. Run the test against a device:

```bash
VEUX_TEST_ESPHOME=satellite.local:6053 VEUX_TEST_ESPHOME_KEY="$VEUX_API_KEY" \
  cargo test -p veux-drivers -- --ignored raw_actions --nocapture
```

### Transport

- The component adds two API actions with responses:
  `xvf_get(resid, cmd, len)` and `xvf_set(resid, cmd, payload)`. The
  `xvf_set` payload is a hex string, for example `"0a1b"`. In ESPHome
  2026.9, an `int[]` argument does not compile with `api.respond`. ESPHome
  2026.11.0 compiles it. The hex string works on both, so it stays.
- `xvf_set` accepts only an allowlist of resource and command IDs: PP 17,
  AEC 33, AUDIO_MGR 35, and the AIC3104 levels (48/11, 48/12). It refuses
  the IDs that the component owns, the XMOS DFU resource (240), and the mute
  function (36/8). A raw API key alone cannot write the XMOS firmware.
- The `xvf_get` response data is `{"reply": [status, payload...]}`. Only
  an I2C failure sets `success = false` and an error message.
- API 1.16 has `ExecuteServiceRequest.return_response` and
  `ExecuteServiceResponse` (message 131). `esphome-client` 0.2.1 has both.
- `EsphomeDevice` is a `ControlPort`, so the USB driver (`Xvf3800`) and its
  encoding, status handling, and retries work over ESPHome unchanged. The
  ESP32 has no copy of the command table. It only moves bytes.
- One entity for each parameter is not used: it would add about 60
  entities to Home Assistant and put the table in YAML.

### I2C framing

- A write is `{resid, cmd, len, payload...}` to address `0x2C`. The
  formatBCE component already has `xmos_write_bytes` for this.
- A read sets bit `0x80` on `cmd`, as on USB. The reply is a status byte,
  then the payload. The component adds a read helper for this.
- The ESP32 does one I2C read for each call and returns the status. veux
  retries a busy status (1 or `0x40`) every 10 ms for up to 1 s, as on
  USB. On a test satellite, a PP parameter read stayed busy for more
  than 5 retries 2 ms apart. A longer wait on the ESP32 would block its
  main loop.

### Discovery

- On connect, veux lists the entities and the actions. When `xvf_get` and
  `xvf_set` are present, the schema is the entities and the USB command
  table for the firmware version that `xvf_get` reads.
- Without them, the link stays in entity-only mode. No config change is
  necessary.
- The formatBCE component writes some XVF3800 parameters from its own
  entities, again at boot and after an XVF3800 reset. A raw write to them
  would not last. Thus veux shows the entity and hides the raw command:
  - the `led` group: the component sets the LED ring 20 times a second;
  - `aec.asroutonoff`, `audio_mgr.op_l`, and `audio_mgr.op_r`: the DSP
    route. `audio_mgr.op_all` sets both outputs, so a write would override
    the route and is hidden too;
  - `aec.asroutgain` and `pp.agcmaxgain`: the ASR output gain and the AGC
    maximum gain entities;
  - `aec.fixedbeamsonoff` and `aec.fixedbeamsazimuth_values`: the beam lock.

  The first design hid the entity instead. On the device, that would lose
  the entity's saved value at each boot.
- `app.usb_bit_depth` is hidden: the I2S firmware reads it as 0, and a
  write reboots the XVF3800.
- A raw parameter is not persistent on this link. The XVF3800 keeps it
  until it reboots, and `commit` does nothing. Run `apply` after a reboot.
- A raw parameter gets the explanation of the same ID on USB.
- A USB profile works on this link, except for the hidden IDs above.
- A full read takes about 1.5 s on a test satellite: one round trip for
  each of about 60 raw parameters.

### Firmware

- The image stays in the ESPHome build: the YAML gives its URL, the build
  embeds it, and an OTA update puts it on the ESP32.
- The config adds the API action `xvf_flash`. It calls
  `start_dfu_update()` of the component: the `respeaker_xvf3800.flash`
  action does not compile in an action with arguments. This holds on ESPHome
  2026.9 and on 2026.11.0: its `play(Ts... x)` does not match the base
  class `play(const Ts &...x)`. The action refuses while an update runs. It does not compare versions.
  formatBCE fixed the signature in their repository (issue 112), so a
  release after 2026.11.2 can use the component action.
- `xvf_flash` has limits that the component gives. `start_dfu_update()` has
  no guard against a second call. The update runs next to the LED, beam,
  and mute polls, which the component does not expect. After a failed update
  the component stops its `loop()` until the next reboot. The firmware
  already updates the XVF3800 at boot when the version differs, so use
  `xvf_flash` only to write the image again.
- The DFU triggers of the component (`on_begin`, `on_progress`, `on_end`,
  `on_error`) set two diagnostic entities: `text_sensor.xvf3800_update`
  (`flashing`, `done`, or `error N`) and `sensor.xvf3800_update_progress`
  (percent).
- veux shows the running version, read with `xvf_get`, and that the image
  comes from the ESPHome build; there are no DFU slots. `F` in the TUI and
  `veux firmware <device> --bundled` run `xvf_flash` after one
  confirmation, and follow the two entities. veux reads only the entities
  during the update, because the XVF3800 does not answer raw reads in DFU.
  The update must start in 15 s and end in 15 minutes.
- Tested on a test satellite on 2026-11-20 (release 2026.11.2): the
  bundled flash ran from 0 to 100 % and the device kept its settings.
  Earlier, with the earlier repository, on 2026-11-17: `veux firmware
  living-room --bundled` wrote the 1.0.7 image again. The progress went from 0 to
  100 %, and after the reset the XVF3800 answered raw reads and the ESP32
  had applied its entity values again.
- Follow-up, if wanted: an action that takes a URL, so the ESP32 gets the
  image over HTTP. Streaming the image over the API is not planned: about
  1 MB through a small RAM over Wi-Fi can leave a half-written image.

### Where the actions live

- The actions are plain ESPHome YAML in the satellite config. They use the
  I2C device of the formatBCE component and its public `read`, `write`, and
  `start_dfu_update`. No external component is necessary.
- If a config cannot reach that I2C device, a fork of formatBCE is the
  fallback.
- An upstream change to formatBCE is not done yet.

## Settings outside the device

"Finished speaking detection" is a Home Assistant select, made by the HA
ESPHome integration. It is not an entity on the device, so the native API
cannot set it.
