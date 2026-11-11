# Changelog

Community firmware for the **DSO-510** handheld oscilloscope (based on the original firmware *APP V2.0.5 250114*) and
the **DSO-510 Link** PC application. The scope reports the firmware version as `usb_link vX.Y`.

## Firmware

### v4.18 - trigger status
- The PC application now learns whether each acquisition was triggered, so it can show the trigger status like a
  bench scope (Trig'd / WAIT / AUTO) and keep the last triggered record in Normal mode.
- Full-memory records carry the scope's vertical scale, so the PC shows them exactly like the scope's screen.

### v4.17 - measurement accuracy
- Frequency is now exact at every time base. Before, it was estimated from the screen and was wrong when the time base
  did not fit the signal (20 kHz at 1 ms/div read 4000 Hz, 50 kHz read 2000 Hz, 10 Hz read 3338 Hz). Now the scope's
  frequency counter is used (2 Hz resolution) whenever the screen cannot resolve the signal, and the screen estimate
  when it is finer or the counter is disturbed by narrow pulses.
- Period and duty cycle are measured at 50 % between minimum and maximum (before: at the trigger level, which gave
  wrong duty cycles on slow AC-coupled pulses, e.g. 27 % instead of 10 %).
- No frequency is shown for noise without a signal; slow signals at fast time bases keep their frequency.
- Tested from 37 Hz to 48 kHz and 10-90 % duty: frequency typically within 0.2 %, duty within 0.7 percentage points.

### v4.11 - v4.16
- Faster text on the screen: the measurement read-outs take about half the time, the display refreshes at about
  110 frames/s instead of 100.
- New diagnostic commands: `PROFD` (display timing), `FREQHW` (frequency counter), `PEEK` / `POKE32` for the LCD bus
  timing (experiments only; the LCD timing itself was not changed).

### v4.9
- Fixed: the `TRIGTEST` diagnostic command could hang the scope.

### v4.8 - faster trigger
- Trigger search about 28x faster; at 50-500 ns/div the screen updates up to twice as fast when the record holds few
  or no edges.
- Fixed: in roll time bases (50 ms/div and slower), with the scope stopped and the horizontal position moved left, the
  scope could show garbage or hang (it searched for a trigger far outside the captured data).

### v4.7 - measurements
- The on-screen measurements (frequency, period, duty, high / low time, Vmax, Vmin, Vpp, mean, RMS, Vpp min) give the
  same values as before but take less time.

### v4.6 - battery percentage
- The battery is shown as **"87%"** with a small battery symbol instead of the 10-step bar. 0 % means the point where
  the scope switches itself off. Green while charging. `BATT 0` / `BATT 1` switch to the original icon and back.
- Fixed: in Single mode at 50 ms/div and slower the captured record was not sent to the PC.

### v4.5
- Faster trace update at 50-500 ns/div.
- More free memory for new functions.

### v4.4 - fast time bases
- Screen update at 50-500 ns/div about 2.5x faster.
- Fixed: at 50 and 100 ns/div the rightmost points of the trace dropped towards the bottom of the screen.
- `FASTBUILD 0|1|2` switches between the new and the original behaviour for comparison.

### v4.3 and earlier
- **USB connection to the PC** (no driver installation needed, or as a COM port): live screen, all settings, full
  remote control.
- **Full memory**: every acquisition's complete record (30100 samples) is sent to the PC.
- **Roll mode** (50 ms/div and slower) streamed to the PC as one long recording.
- **Battery status** sent to the PC.
- Screen update at 250 ns/div from 12 to 57 frames/s.
- The signal generator can be used up to 50 kHz while the oscilloscope screen is shown (before: 1 kHz).
- Fixed: random frequency values shown without a signal.
- Fixed: rare memory corruption between the display and the acquisition.
- Diagnostic commands: `PEEK`, `POKE8`, `PROF`, `TRIGTEST`, `DIAG`, `INFO`.

### Known limitations
- The time base runs 4.17 % slower than its labels; the automatic measurements and the PC application compensate.
- In Single mode at 50-500 ns/div the sampling continues after the trigger.
- The generator cannot make every frequency exactly (e.g. 47000 Hz -> 48000 Hz); the scope shows the real frequency.
- Vertical calibration: the original firmware stores a vertical scale in its settings. If the settings are empty, the
  first press of AUTO measures it from the screen, and with no proper signal the result can be wrong: all voltages
  are then shown too large or too small on every range (seen: 1.5x too large). Check with a known voltage, e.g. a
  battery; a scope that reads correctly needs nothing, otherwise use DSO-510 Calibrate (see the README).

## DSO-510 Link (PC application)

### New: frequency response
- **Frequency response** view: Bode magnitude plot with the scope's own generator, 10 Hz - 50 kHz. Step 1 "Reference"
  (probe on the circuit input), step 2 "Measure" (probe on the output); the plot shows the gain in dB with the maximum
  and the -3 dB frequencies marked, a read-out under the mouse and CSV export. Instructions with connection diagrams
  are in the panel ("How to use"). The scope and generator settings are restored after a sweep.
- Accuracy: with the probe directly on the generator the result is flat within -0.11 ... +0.23 dB from 20 Hz to 50 kHz.

### New: DSO-510 Calibrate
- A separate program (`bin/DSO-510-Calibrate.exe`) that corrects the voltage readings with one DC voltage you know:
  optional zero measurement, measurement on every V/div range where the voltage fits, a table of what the scope
  showed and will show, writing the new factor and checking the screen afterwards. Step-by-step instructions in
  the window (English, Polish), warnings for a probe at x10, no contact or a changing signal; undo buttons.

### Trigger status and full memory
- Trigger status in the corner of the plot, like a bench scope: **Trig'd** (triggered), **WAIT** (Normal or Single
  without a trigger: the last triggered waveform stays on the screen), **AUTO** (Auto without a trigger: free running).
- Normal mode in Full memory: records without a trigger are no longer shown; before, they appeared shifted to the
  start of the record ("trace from the left edge to the middle").
- Full memory uses the same vertical scale as the scope's screen: voltages, the trigger level line and the trigger
  point now agree with the Screen view. The raw export (.bin) still holds the unchanged ADC samples.

### Other changes
- Single mode at 50 ms/div and slower shows the captured record.
- Battery shown in the top bar (level, voltage, charging).
- Connection diagrams: labels no longer overlap the wires.

### Features
- Live screen with persistence; trigger level, offset and position dragged with the mouse.
- Full memory: all 30100 samples of every acquisition, zoom and pan, measurements at full resolution, history of
  200 records; roll mode recorded as one long stream.
- FFT with several windows, harmonics, THD, SFDR, noise floor.
- Measurements: Vpp, Vmax, Vmin, Vavg, Vrms, amplitude, frequency, period, duty, rise / fall time, overshoot,
  statistics.
- Time and voltage cursors; history of 5000 frames; sessions; export to PNG, CSV and the raw ADC buffer.
- Full remote control of the scope and the generator; virtual front panel.
- Polish and English user interface, help tips, crash log.
