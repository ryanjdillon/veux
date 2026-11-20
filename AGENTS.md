# veux

Rust terminal UI that configures ReSpeaker devices (XVF3800, 2-Mics Pi HAT
v1.x) over local USB, SSH, and ESPHome. Desired state is in `veux.toml`.

## Tech stack

- Rust (version in `rust-toolchain.toml`), Cargo workspace in `crates/`.
- `ratatui` for the TUI, `rusb` for XVF3800 USB control.
- Nix dev shell supplies the toolchains only.

## Common commands

```bash
just check   # fmt, clippy, tests, cargo deny
just fmt
```

## Architecture

See `docs/spec.md`.

- `veux-model`: schema, values, config, validation, three-state diff. No I/O
  except reading the config file.
- `veux-drivers`: one module for each device kind. Each declares a `KindSpec`.
  `esphome` is a link, not a kind: its schema comes from the device. The
  `Device` trait is the one interface to an open device; `open_local` opens a
  device on the machine that runs it.
- `veux-rpc`: the helper protocol (JSON lines), the server loop, and
  `RemoteDevice`, which implements `Device` over a helper session.
- `veux`: the binary (CLI, TUI, and `helper` mode). `connection.rs` opens a
  device: local, ESPHome, or remote. `remote.rs` starts the helper over SSH
  and uploads it. `tui/app.rs` holds the state and key handling, and talks
  to devices only through the `Device` trait, and to firmware through the
  `Firmware` trait in `connection.rs`. `tui/ui.rs` only draws.

## Invariants

- The TUI renders forms from the driver schema. It has no code for a
  specific device kind.
- A driver does not know about SSH. A transport does not know about
  parameters.
- `apply` and live edits write runtime values only. Only `commit` writes
  flash or `asound.state`.
- Do not add a parameter to a schema before it is verified on a device or in
  the vendor command table.
- veux owns the ALSA mixer values on the Pi satellites. Their NixOS config
  only persists them (`alsa-restore`). ESPHome and Home Assistant own the
  XVF3800 entities; veux edits them through the API, not the firmware YAML.

## Drift discipline

| When you change | Also update |
| --- | --- |
| `veux.toml` format (`crates/veux-model/src/config.rs`) | `docs/spec.md`, `examples/veux.toml` |
| Driver interface or crate layout | `docs/spec.md`, `docs/architecture.md`, this file |
| Helper protocol (`crates/veux-rpc/`) | `docs/spec.md`; change `PROTOCOL_VERSION` when a message changes |
| CLI commands | `README.md`, `docs/spec.md` |
| XVF3800 command table or protocol | `docs/devices/xvf3800.md` |
| ESPHome link (`crates/veux-drivers/src/esphome/`) | `docs/devices/esphome.md` |
| 2-Mic driver (`crates/veux-drivers/src/respeaker_2mic_v1/`) | `docs/devices/respeaker-2mic.md` |
| A parameter is added or changes meaning | its entry in `crates/veux-drivers/src/explanations/` (`docs/development/explanations.md`) |

## Known issues

- The XVF3800 byte order and some ranges are not confirmed on hardware. See
  the open items in `docs/devices/xvf3800.md`.

# SYSTEM.md

<!-- Rules for all repos, not only this repo. A periodic process moves them to
     SYSTEM.md in ryanjdillon/agents. Keep this heading. See docs/promotion.md
     in that repo. -->
