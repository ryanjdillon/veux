# Supported devices

The `esphome` link is the ESPHome link of the XVF3800. It is not a separate kind.
The XVF3800 satellite firmware is formatBCE [Respeaker-ESPHome-integration](https://github.com/formatBCE/Respeaker-ESPHome-integration). It needs three extra API actions for the raw parameters and DFU. See [xvf-actions.yaml](../../examples/esphome/xvf-actions.yaml).

| Kind                                           | Hardware                              | Links                              |
|---                                             |---                                    |---                                 |
| `xvf3800` ([doc](xvf3800.md), [ESPHome](esphome.md)) | ReSpeaker XVF3800 4-mic array   | `usb` (local or remote), `esphome` |
| `respeaker-2mic-v1` ([doc](respeaker-2mic.md)) | ReSpeaker 2-Mics Pi HAT v1.x (WM8960) | `alsa` (remote)                    |

## Add support for a device

1. Add a driver module in `crates/veux-drivers/src/`. Declare the module in `lib.rs`.
2. In the module, write a `kind()` function that returns a `KindSpec`. Add it to `catalog()` in `lib.rs`.
3. Implement the `Device` trait for the device. See "Driver interface" in [spec.md](../spec.md).
4. Open the device in `open_local` in `crates/veux-drivers/src/device.rs`.
5. Add parameters to the schema only after you verify them on a device or in the vendor command table.
6. Add an explanation for each parameter in `crates/veux-drivers/src/explanations/`. Follow [explanations.md](../development/explanations.md).
7. Add a doc page in this directory, and link it from this index.
8. Update the docs that the drift table in [AGENTS.md](../../AGENTS.md) names.
