# DSO-510 community firmware and PC application

Improved firmware for the **DSO-510** handheld oscilloscope and **DSO-510 Link**, a Windows application that shows and
controls the scope over USB. See [CHANGELOG.md](CHANGELOG.md) for everything that changed.

| Path | Content |
|---|---|
| `firmware/APP_DSO-510_v4.18.bin` | firmware image (based on the original APP V2.0.5 250114) |
| `bin/DSO-510-Link.exe` | PC application, ready to run (Windows 10/11, x64) |
| `app/` | source code of the PC application (Rust) |
| `SHA256SUMS` | checksums of both binaries |

## Screenshots

**Screen**: the live trace with measurements and all scope settings.

![Screen view](docs/screenshots/screen.png)

**Full memory**: every one of the 30100 samples of each acquisition, with zoom and pan; the strip on top shows
the whole record and the blue band the part the scope shows.

![Full memory view](docs/screenshots/full_memory.png)

**FFT** of the full record, with the harmonics marked.

![FFT](docs/screenshots/fft.png)

**Frequency response**: a Bode magnitude plot made with the scope's own generator. Instructions and connection
diagrams are in the panel.

![Frequency response with instructions](docs/screenshots/frequency_response_help.png)

![Frequency response result](docs/screenshots/frequency_response.png)

**Help tips**: the battery icon and every `[?]` explain themselves when the mouse rests on them.

![Tooltips](docs/screenshots/tooltips.png)

## Flashing the firmware
1. Switch the scope off. Hold the update button while switching it on: the scope appears as the USB drive
   **"AT32 MSC IAP"** (the bootloader, not changed by this firmware).
2. Copy `firmware/APP_DSO-510_v4.18.bin` to that drive and wait until the scope restarts.
3. Going back to the original firmware works the same way with the original image.

The firmware is a full image: the original firmware with patches. It only works on the DSO-510 with the AT32F403A.

## Using the PC application
1. Connect the scope with a USB cable (data cable). With this firmware it appears as a WinUSB device, no driver needed.
2. Start `bin/DSO-510-Link.exe`; it finds the scope by itself.
3. Views in the top bar: **Screen** (live trace), **Full memory** (every sample of each acquisition),
   **Frequency response** (Bode plot with the built-in generator; instructions and connection diagrams in the panel
   on the right under "How to use").
4. Language: menu 🌐 (Polish, English). Exports go to `Documents\DSO-510`.

## Building the application
```
cd app
cargo build --release        # target\release\dso510-link.exe
cargo test --release         # unit tests and UI tests
```
Requires Rust (edition 2024). The `examples/` folder holds the device test tools used during development (profiling,
trigger tests, measurement matrix, frequency response test); they talk to a connected scope.

## Notes
- New diagnostic commands of the firmware (`PROF`, `PROFD`, `TRIGTEST`, `FASTBUILD`, `FREQHW`, `BATT`, `PEEK`, ...)
  are text lines sent over the USB link. The application offers INFO / DIAG / GET under "Front panel & advanced";
  the tools in `app/examples/` (e.g. `prof`, `trigtest`, `freqhw`) send the others.
- Use at your own risk. The bootloader is untouched, so the original firmware can always be restored.
