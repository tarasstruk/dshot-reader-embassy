## DShot reader based on embassy_rp

This binary crate is part of a test framework for DShot protocol communication.

It is a complementary side to [dshot-writer-test](https://github.com/tarasstruk/dshot-writer-test)
and implements its "reading counterpart".

This package uses  [embassy-rp](https://crates.io/crates/embassy-rp) dependency to manage async tasks with Rust
and provides a "npn-blocking" alternative to [dshot-reader-test](https://github.com/tarasstruk/dshot-reader-test).

It is being moved to bidirectional DShot 300 (see `docs/action-plan.md`). Currently it receives
inverted bidirectional frames (the line idles high) and does not reply yet.

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
- RP2040 GPIO0 to the source of DShot signals;
- RP2040 GPIO3 can be watched as debug output.

The received frames are decoded with `dshot-codec` and reported as text over the USB connection
using a CDC+ACM profile, twice a second:

```
7D05 thr=1000 T=0 cmd=0 n=12345 bad=0
```

- `7D05` — the latest frame;
- `thr` — throttle, `T` — telemetry request bit, `cmd` — 1 for special commands (throttle 0–47);
- `n` — all frames received, `bad` — frames with a bad CRC (shown as `7D04 BAD_CRC ...`).

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

Debug-signal on the pin GPIO3 pulses high at the moment each bit of a DShot frame is sampled
(about 1.67 µs after the falling edge of the bit).
