# Parameter explanations

The TUI detail pane explains what the highlighted parameter does. Devices
report too little for this: an ESPHome entity reports only its name. So veux
has curated explanations in `crates/veux-drivers/src/explanations/`.

## Format

One TOML table for each parameter. The key is `<kind>/<link>/<parameter id>`:

```toml
["xvf3800/esphome/switch.beam_lock"]
text = """
What the parameter does, what each value means in practice, the trade-off,
and when to change it.
"""
source = "https://..."
```

- `esphome.toml`: the formatBCE XVF3800 satellite entities (packages at
  542a05d), plus `number.output_gain` from the satellite firmware config.
- `xvf3800-usb.toml`: the XVF3800 USB commands in `xvf3800/commands.rs`.
  A raw XVF3800 parameter on the ESPHome link has the same ID, so it gets
  the USB entry when `esphome.toml` has none.
- `wm8960.toml`: the WM8960 mixer controls of the 2-Mics Pi HAT v1.x.
- `apa102.toml`: the APA102 LED parameters of the 2-Mics Pi HAT v1.x.

Lines in a paragraph are joined when veux loads the file; a blank line
starts a new paragraph.

## Rules

- Each entry comes from the source it cites. Mark an inference with
  "(inferred)". Write "unknown" when the sources do not say; never guess.
- Give the default when a source states it. If sources disagree (XMOS and
  Seeed often do), give both and say so.
- Simplified Technical English, about 90 words at most.
- Do not repeat how the value persists: the pane shows that separately.
- A test checks that every key of the USB and ALSA files names a real
  parameter. ESPHome keys can only be checked for their form, because the
  entities come from the device.
