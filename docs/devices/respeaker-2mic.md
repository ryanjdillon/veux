# ReSpeaker 2-Mics Pi HAT v1.x

This doc records the facts that the `respeaker-2mic-v1` driver depends on.

## Requirements

| Need | Detail |
|------|--------|
| Access | SSH to the Pi. |
| ALSA tools on the Pi | `alsactl` and `amixer`. |
| veux helper | veux uploads its static helper to the Pi. Set `helper_path` to use a helper that is already on the host. |
| Values that last over a reboot | A boot step that loads `asound.state`. The NixOS hosts of the author use `hardware.alsa.enablePersistence`. Other systems need an equivalent. This is not verified on other systems. See [Persistence](#persistence). |
| LEDs | The SPI overlay, and a writable `/dev/spidev0.0` (group `spi` on the satellites). See [LEDs](#leds). |

## Hardware

- The HAT v1.x has a Wolfson WM8960 codec at I2C address `0x1a`, and three
  APA102 LEDs on SPI. The HAT V2.0 has a TLV320AIC3104 codec and needs a
  different driver kind.
- On the satellite hosts the ALSA card is `respeaker2mic` (`hw:respeaker2mic`).
  HDMI is card 1 and the bcm2835 headphone output is card 2. Select a card
  other than the default with `match = { card = "<name>" }`.

## How the driver works

The driver runs on the host that has the HAT, inside the helper. It runs the
ALSA tools and does not link libasound, so the helper stays a static binary.

| Operation | Command |
|-----------|---------|
| Read all  | `alsactl -f - store <card>`, parsed by `alsa_state.rs` |
| Write     | `amixer -c <card> -q cset numid=<n> <value>` |
| Persist   | `alsactl store <card>`; with `sudo -n` when not root |
| Meter     | `arecord -D hw:CARD=<card>,DEV=0 -f S16_LE -r 16000 -c 2 -t raw`, read in 100 ms blocks |
| Test tone | `timeout 20 speaker-test -D hw:CARD=<card>,DEV=0 -c 2 -r 48000 -t sine -f <hz> -l 1 [-s 1\|2]` |

A write uses the numeric control ID, so control names with quotes or commas
cause no problems.

## Parameters

A parameter ID is `mixer.<slug of the ALSA control name>`, for example
`mixer.capture_volume`. The description shows the dB range when the
control has one.

| ALSA type          | veux type                          |
|--------------------|------------------------------------|
| INTEGER, 1 channel | integer, with the control range    |
| INTEGER, 2 channels| vector of 2, as `[left, right]`    |
| BOOLEAN, 1 channel | bool                               |
| BOOLEAN, 2 channels| vector of 2, 0 or 1                |
| ENUMERATED         | enum of the item names             |

Other types, for example IEC958, are not parameters.

The static schema for `veux check` comes from
`crates/veux-drivers/tests/fixtures/respeaker2mic.state`. That file is an
`alsactl` dump of the HAT on a test satellite (59 controls). When veux
connects, it uses the controls that the card reports.

## Persistence

The NixOS satellite hosts set `hardware.alsa.enablePersistence`. Thus
`alsa-restore` loads `/var/lib/alsa/asound.state` at boot and stores it at
shutdown. The host config sets no mixer values: veux owns them. Only root
can write the state file, so `persist` uses `sudo -n`. The satellite user
has sudo without a password.

## Capture gain

With the default mixer, the input mixer boost switches
(`mixer.left_input_mixer_boost_switch`, `mixer.right_input_mixer_boost_switch`)
are off. Then the microphones do not reach the ADC, and captures are almost
silent.

Measured on a test satellite on 2026-11-13, with speech at normal distance
and the first second of each capture dropped (it has a start-up thump):

| Boost (LINPUT1/RINPUT1) | Capture Volume | Speech | Peak | Floor |
|---|---|---|---|---|
| 1 (+13 dB) | 39 (+12 dB) | -43 dBFS | -36 dBFS | -48 dBFS |
| 3 (+29 dB) | 47 (+18 dB) | -15 dBFS | -3.5 dBFS | -29 dBFS |
| 3 (+29 dB) | 43 (+15 dB) | about -18 dBFS | about -6 to -8 dBFS | about -32 dBFS |

The working point is the last row: both boost switches on, boost 3, and
Capture Volume 43, with ALC and the noise gate off, because the voice
pipeline does its own gain control. "Speech" is the RMS of the loudest 20 %
of 50 ms frames. The last row is computed from the second (a linear 3 dB
step), not measured.

## Playback

- With the default mixer, the output mixer PCM switches
  (`mixer.left_output_mixer_pcm_playback_switch`,
  `mixer.right_output_mixer_pcm_playback_switch`) are off, and playback is
  silent.
- The JST speaker connector is on the right speaker channel (SPK_RP/SPK_RN).
  The codec powers both speaker channels together, so the device tree route
  to SPK_LP/SPK_LN is enough.
- The 3.5 mm jack is the headphone output (HP_L/HP_R). Into a stereo, set
  `mixer.headphone_playback_volume` near 0 dB (121) and keep
  `mixer.playback_volume` below full scale.
- The default volumes put a test tone about 40 dB below full scale, which is
  hard to hear on the speaker. `mixer.playback_volume` 235 (-10 dB) and
  `mixer.speaker_playback_volume` 115 (-6 dB) were audible but a little
  quiet (2026-11-13).

## Meter and test tone

- The meter shows MIC1 as the left channel and MIC2 as the right channel.
  It opens the `hw:` device directly, so it fails when another program,
  such as the voice satellite, holds the capture. Stop that program first.
- Each capture start gives a thump of about one second at full scale. The
  meter ignores the first second.
- The tone plays one pass of `speaker-test`: one channel with `-s`, else
  both in turn. Its level follows `mixer.playback_volume` and the speaker or
  headphone volume.

## LEDs

The HAT has three APA102 LEDs on SPI0. The host needs the SPI overlay and
`/dev/spidev0.0`, writable by the user that runs the helper (group `spi` on
the satellites). Without the device, the LED parameters are not in the
schema.

| Parameter        | Type                   | Meaning                          |
|------------------|------------------------|----------------------------------|
| `led.power`      | bool                   | LEDs on or off                   |
| `led.brightness` | integer, 0 to 31       | APA102 global brightness         |
| `led.rgb`        | 3 integers, 0 to 255   | Color of all three LEDs          |

- A write sends one APA102 frame: 32 zero bits, then each LED as `111` and
  5 brightness bits, blue, green, red, then 32 one bits.
- A plain write to the spidev node uses the device-tree speed (8 MHz) and
  SPI mode 0.
- The LEDs cannot be read back. The helper keeps the last written state in
  `~/.cache/veux/leds.toml` on the host. Without that file the runtime value
  is unknown.
- The LEDs go dark at power off, so `commit` does not apply to them.
