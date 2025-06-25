## DShot reader based on embassy_rp

This binary crate is part of a test framework for DShot protocol communication.

It is a complementary side to [dshot-writer-test](https://github.com/tarasstruk/dshot-writer-test)
and implements its "reading counterpart".

This package uses  [embassy-rp](https://crates.io/crates/embassy-rp) dependency to manage async tasks with Rust
and provides a "npn-blocking" alternative to [dshot-reader-test](https://github.com/tarasstruk/dshot-reader-test).

It is actually tuned for uni-directional DShot 300 protocol.

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

The received messages are not decoded and just sent "as is" over the USB connection using a CDC+ACM profile.
The messages as bytes can be seen in terminal, for example:

```shell
tio --output-mode=hex4 /dev/tty.usbmodem123456781
```

### Debugging

Debug-signal on the pin GPIO3 pulls up for each
logical "1" detected on a DShot frame.
