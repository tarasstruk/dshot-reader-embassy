## DShot reader based on embassy_rp

This binary crate is part of a test framework for DShot protocol communication.

It is a complementary side to [dshot-writer-test](https://github.com/tarasstruk/dshot-writer-test)
and implements its "reading counterpart".

This package uses  [embassy-rp](https://crates.io/crates/embassy-rp) dependency to manage async tasks with Rust
and provides a "npn-blocking" alternative to [dshot-reader-test](https://github.com/tarasstruk/dshot-reader-test).

It is being moved to bidirectional DShot 300 (see `docs/action-plan.md`). Currently it receives
inverted bidirectional frames (the line idles high) and ~30 µs after each frame replies on the same
wire with telemetry that grows linearly with throttle (stage C):
`eRPM = 1000 + throttle × 20`, e.g. throttle 1000 → 21 000 eRPM (3 000 RPM in Betaflight with 14 poles).
Special commands (throttle 0–47) get "motor stopped"; after a frame with a bad CRC the reply
for the last good frame is kept.

### Hardware

The binary is tested on RP2040 microcontroller board.

A source of DShot signal with level `3.3V` is required. 
It can be any ordinary Flight Controller or the [dshot-writer-test](https://github.com/tarasstruk/dshot-writer-test).

### Installation

The compiled binary could be installed to PR2040 when it's
connected to the host computer as USB mass storage.
Then run `cargo r`.

### Connection

Connect these pins:

- RP2040 signal ground;
- RP2040 GPIO0 to the DShot signal line (it is both read and driven: the reply goes back to the FC);
- RP2040 GPIO3 can be watched as debug output.

GPIO0 has the internal pull-up enabled; the line idles high.

The received frames are decoded with `dshot-codec` and reported as text over the USB connection
using a CDC+ACM profile, twice a second:

```
7D05 thr=1000 T=0 cmd=0 n=12345 bad=0 txf=0 late=0 gap=251 edt=0
```

- `7D05` — the latest frame;
- `thr` — throttle, `T` — telemetry request bit, `cmd` — 1 for special commands (throttle 0–47);
- `n` — all frames received, `bad` — frames with a bad CRC (shown as `7D04 BAD_CRC ...`);
- `txf` — replies that did not fit into the PIO TX FIFO (should stay 0).
- `late` — times the PIO task woke up more than 1 ms after the previous time (the RX FIFO
  holds 4 frames, so later the PIO drops frames); `gap` — the longest time between two
  wake-ups since the previous line, µs (≈ 250 at 4 kHz);
- `edt` — 1 when the FC has turned extended telemetry (EDT) on: DShot command 13 six times
  in a row, answered once with the EDT version frame `0xE02`; command 14 turns it off
  (answered with `0xEFF`). With EDT on, every 64th reply carries a temperature (fixed 42 °C)
  or a current (0 A at throttle 48 to 20 A at 2047), in turn, instead of the period
  (see `docs/edt.md`).

The lines can be seen in terminal, for example:

```shell
tio /dev/tty.usbmodem123456781
```

### Tests

The hardware-independent encoding lives in the `dshot-codec` crate and is tested on the host:

```shell
cargo test-codec
```

### Debugging

Watch GPIO0 with a logic analyzer: after each frame from the FC there should be a pause of ~30 µs
and a 21-bit reply of 2.667 µs per bit (≈ 56 µs). For 30 000 eRPM (`e12 = 0x5F4`) the reply levels are
`0 11001101010100101101` (see `docs/reply-waveform-0x5F4.md`).

Debug output on GPIO3 (PIO side-set):

- a ~170 ns pulse around each sampled bit of a frame (about 1.67 µs after the falling edge of the bit);
- high for the whole time the RP2040 drives GPIO0, i.e. while the reply is being sent (≈ 56 µs).
