//! Hardware-independent encoding for bidirectional DShot.
//!
//! - [`decode_frame`] turns a value from the PIO RX FIFO into a [`Frame`] from the FC.
//! - [`encode_frame`] is the reverse of [`decode_frame`], as done by the FC.
//! - [`frame_ok`] checks the inverted CRC of a 16-bit frame received from the FC.
//! - [`encode_period`] packs an electrical revolution period into the 12-bit `e12` format.
//! - [`encode_temperature`], [`encode_voltage`], [`encode_current`], [`encode_status`],
//!   [`encode_debug1`] and [`encode_debug2`] pack extended telemetry (EDT) into `e12`.
//! - [`decode_telemetry`] tells what an `e12` carries: a period or an EDT value.
//! - [`EdtSwitch`] turns EDT on and off from repeated DShot commands 13 and 14.
//! - [`encode_reply`] turns `e12` into the line levels of the telemetry reply,
//!   ready to be pushed into the PIO TX FIFO.
//! - [`decode_reply`] is the reverse of [`encode_reply`], as done by the FC.
//!
//! See `docs/reply-waveform-0x5F4.md` for a step-by-step walkthrough.
#![cfg_attr(not(test), no_std)]

/// GCR code (5 bits) for each nibble.
pub const GCR: [u8; 16] = [
    0x19, 0x1B, 0x12, 0x13, 0x1D, 0x15, 0x16, 0x17, //
    0x1A, 0x09, 0x0A, 0x0B, 0x1E, 0x0D, 0x0E, 0x0F,
];

/// `e12` value meaning "motor stopped" (also the slowest representable period).
pub const E12_STOPPED: u16 = 0x0FFF;

/// Number of line levels in a reply: start bit + 20 GCR bits.
pub const REPLY_BITS: u32 = 21;

/// Reply levels are left-aligned in the TX FIFO word (bits 31..11),
/// because the PIO shifts OSR to the left.
const REPLY_SHIFT: u32 = 32 - REPLY_BITS;

/// Inverse of [`GCR`]: 5-bit code → nibble, `None` for codes that are not in the table.
const GCR_INV: [Option<u8>; 32] = {
    let mut inv = [None; 32];
    let mut n = 0;
    while n < 16 {
        inv[GCR[n] as usize] = Some(n as u8);
        n += 1;
    }
    inv
};

/// Inverted 4-bit CRC used by bidirectional DShot, over the upper 12 bits of a frame
/// or over `e12` of a reply.
fn crc4_inverted(v: u16) -> u16 {
    !(v ^ (v >> 4) ^ (v >> 8)) & 0x0F
}

/// Checks the inverted CRC of a 16-bit frame from the FC (bidirectional mode).
///
/// `frame` is the logical frame, i.e. already inverted back from the line levels.
pub fn frame_ok(frame: u16) -> bool {
    crc4_inverted(frame >> 4) == frame & 0x0F
}

/// Throttle values below this are special commands (0 = motor stop, 1..=47 = beep, version
/// request etc.); from this value up they are real throttle.
pub const THROTTLE_MIN: u16 = 48;

/// Largest throttle value (11 bits).
pub const THROTTLE_MAX: u16 = 0x7FF;

/// A frame from the FC: 11-bit throttle, telemetry request bit T and a 4-bit CRC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// 0..=2047; below [`THROTTLE_MIN`] it is a special command.
    pub throttle: u16,
    /// The T bit: the FC asks for extended telemetry.
    pub telemetry: bool,
}

impl Frame {
    /// True for special commands (throttle 0..=47), false for real throttle.
    pub fn is_command(&self) -> bool {
        self.throttle < THROTTLE_MIN
    }
}

/// Reasons [`decode_frame`] rejects a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The CRC does not match.
    BadCrc,
}

/// Decodes a value from the PIO RX FIFO into a [`Frame`].
///
/// The PIO samples the line in the middle of each bit, and in bidirectional mode
/// the line is low there for a 1 and high for a 0. So `raw` holds the 16 frame bits
/// inverted, in the lower 16 bits (autopush at 16). They are inverted back here.
pub fn decode_frame(raw: u32) -> Result<Frame, FrameError> {
    let frame = !raw as u16;
    if !frame_ok(frame) {
        return Err(FrameError::BadCrc);
    }
    Ok(Frame {
        throttle: frame >> 5,
        telemetry: (frame >> 4) & 1 == 1,
    })
}

/// Encodes a frame as the FC sends it: throttle (11 bits), T, inverted CRC (4 bits).
///
/// Returns the logical 16-bit frame, MSB first on the line. Throttle above
/// [`THROTTLE_MAX`] is cut to 11 bits.
pub fn encode_frame(frame: Frame) -> u16 {
    debug_assert!(
        frame.throttle <= THROTTLE_MAX,
        "throttle {} > 2047",
        frame.throttle
    );
    let v = ((frame.throttle & THROTTLE_MAX) << 1) | frame.telemetry as u16;
    (v << 4) | crc4_inverted(v)
}

/// Packs an electrical revolution period (µs) into `e12`: 3 bits of exponent `e`
/// and 9 bits of mantissa `m`, period = `m << e`.
///
/// Returns [`E12_STOPPED`] for a zero period and for periods too long to represent
/// (≥ 65 408 µs, i.e. ≤ 917 eRPM; see `docs/erpm-lower-limit.md`).
pub fn encode_period(period_us: u32) -> u16 {
    if period_us == 0 {
        return E12_STOPPED;
    }
    let mut m = period_us;
    let mut e = 0;
    while m > 0x1FF {
        m >>= 1;
        e += 1;
    }
    if e > 7 {
        return E12_STOPPED;
    }
    ((e << 9) | m) as u16
}

/// Unpacks `e12` into the period in µs (`m << e`).
pub fn decode_period(e12: u16) -> u32 {
    ((e12 & 0x1FF) as u32) << ((e12 >> 9) & 0x07)
}

// Extended DShot telemetry (EDT, https://github.com/bird-sanctuary/extended-dshot-telemetry)
// reuses the `e12` slot of the reply. A period always has the top mantissa bit (bit 8) set
// when `e` > 0, because `encode_period` halves it until it is 256..=511. So values with
// `e` > 0 and bit 8 clear are free: EDT puts a 4-bit type `eee0` in bits 11..8 and an
// 8-bit value in bits 7..0. The FC turns EDT on with DShot command 13.

/// EDT type of a temperature value, °C.
const EDT_TEMPERATURE: u16 = 0x2;
/// EDT type of a voltage value, 0.25 V steps.
const EDT_VOLTAGE: u16 = 0x4;
/// EDT type of a current value, A.
const EDT_CURRENT: u16 = 0x6;
/// EDT type of debug value 1.
const EDT_DEBUG1: u16 = 0x8;
/// EDT type of debug value 2.
const EDT_DEBUG2: u16 = 0xA;
/// EDT type of a status value (flags and max stress). The version frame and
/// "EDT off" ([`E12_EDT_DISABLED`]) use the same type; the FC tells them apart by
/// context, as a reply to DShot command 13 or 14.
const EDT_STATUS: u16 = 0xE;

/// EDT status: event flags and the highest stress level since the last status frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    /// Bit 7: alert event.
    pub alert: bool,
    /// Bit 6: warning event.
    pub warning: bool,
    /// Bit 5: error event.
    pub error: bool,
    /// Bits 3..0: max stress level, 0–15. Bit 4 is unused.
    pub max_stress: u8,
}

/// What an `e12` value of a reply carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Telemetry {
    /// Electrical revolution period, µs (see [`encode_period`]).
    Period(u32),
    /// Temperature, °C (0–255).
    Temperature(u8),
    /// Voltage in 0.25 V steps (0–255, i.e. 0–63.75 V).
    Voltage(u8),
    /// Current, A (0–255).
    Current(u8),
    /// Debug value 1 (0–255), meaning up to the ESC.
    Debug1(u8),
    /// Debug value 2 (0–255), meaning up to the ESC.
    Debug2(u8),
    /// Status flags and max stress (also what the version frame decodes to).
    Status(Status),
    /// Any other EDT type (stress level `0xC`): `kind` is the 4-bit type from
    /// bits 11..8, `value` the lower 8 bits.
    Other { kind: u8, value: u8 },
}

/// Packs a temperature (°C) into `e12` as EDT: type `0x2`, e.g. 50 °C → `0x232`.
pub fn encode_temperature(celsius: u8) -> u16 {
    (EDT_TEMPERATURE << 8) | celsius as u16
}

/// Packs a voltage in 0.25 V steps into `e12` as EDT: type `0x4`,
/// e.g. 15.25 V = 61 steps → `0x43D`.
pub fn encode_voltage(quarter_volts: u8) -> u16 {
    (EDT_VOLTAGE << 8) | quarter_volts as u16
}

/// Packs a current (A) into `e12` as EDT: type `0x6`, e.g. 17 A → `0x611`.
pub fn encode_current(amps: u8) -> u16 {
    (EDT_CURRENT << 8) | amps as u16
}

/// Packs debug value 1 into `e12` as EDT: type `0x8`.
pub fn encode_debug1(value: u8) -> u16 {
    (EDT_DEBUG1 << 8) | value as u16
}

/// Packs debug value 2 into `e12` as EDT: type `0xA`.
pub fn encode_debug2(value: u8) -> u16 {
    (EDT_DEBUG2 << 8) | value as u16
}

/// Packs a status into `e12` as EDT: type `0xE`, e.g. warning + stress 11 → `0xE4B`.
/// `max_stress` above 15 is cut to 4 bits.
pub fn encode_status(status: Status) -> u16 {
    debug_assert!(status.max_stress <= 0x0F);
    let flags =
        (status.alert as u16) << 7 | (status.warning as u16) << 6 | (status.error as u16) << 5;
    (EDT_STATUS << 8) | flags | (status.max_stress & 0x0F) as u16
}

/// Tells what `e12` carries, as the FC does: a period when the 4-bit type is 0
/// or bit 8 is set, otherwise an EDT value.
pub fn decode_telemetry(e12: u16) -> Telemetry {
    let kind = (e12 >> 8) & 0x0F;
    if kind == 0 || kind & 1 == 1 {
        return Telemetry::Period(decode_period(e12));
    }
    let value = (e12 & 0xFF) as u8;
    match kind {
        EDT_TEMPERATURE => Telemetry::Temperature(value),
        EDT_VOLTAGE => Telemetry::Voltage(value),
        EDT_CURRENT => Telemetry::Current(value),
        EDT_DEBUG1 => Telemetry::Debug1(value),
        EDT_DEBUG2 => Telemetry::Debug2(value),
        EDT_STATUS => Telemetry::Status(Status {
            alert: value & 0x80 != 0,
            warning: value & 0x40 != 0,
            error: value & 0x20 != 0,
            max_stress: value & 0x0F,
        }),
        _ => Telemetry::Other {
            kind: kind as u8,
            value,
        },
    }
}

/// DShot command: turn extended telemetry on.
pub const CMD_EDT_ENABLE: u16 = 13;
/// DShot command: turn extended telemetry off.
pub const CMD_EDT_DISABLE: u16 = 14;
/// A command that changes ESC state takes effect only after this many identical frames
/// in a row, so that a single corrupted frame cannot change it.
pub const COMMAND_REPEATS: u8 = 6;
/// EDT version this ESC reports when EDT is turned on (spec v2).
pub const EDT_VERSION: u8 = 2;
/// `e12` sent once when EDT is turned off.
pub const E12_EDT_DISABLED: u16 = 0xEFF;

/// Packs the EDT version frame, sent once when EDT is turned on: type `0xE`, e.g. `0xE02`.
pub fn encode_edt_version(version: u8) -> u16 {
    0xE00 | version as u16
}

/// Turns EDT on and off from DShot commands [`CMD_EDT_ENABLE`] and [`CMD_EDT_DISABLE`],
/// each acting after [`COMMAND_REPEATS`] identical frames in a row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EdtSwitch {
    enabled: bool,
    /// Throttle field of the previous frame and how many times in a row it came.
    last: u16,
    repeats: u8,
}

impl EdtSwitch {
    /// A switch with EDT already on, without DShot command 13. For an FC that decodes EDT
    /// without asking for it (Betaflight `dshot_edt = FORCE` never sends command 13).
    /// Command 14 still turns it off.
    pub fn enabled_at_start() -> Self {
        EdtSwitch {
            enabled: true,
            ..Self::default()
        }
    }

    /// Whether EDT is on.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Feeds a frame with a good CRC. Returns the `e12` of a one-off reply when a command
    /// takes effect: the version frame for "on", [`E12_EDT_DISABLED`] for "off".
    ///
    /// The command acts on exactly the [`COMMAND_REPEATS`]-th frame in a row; further
    /// repeats (Betaflight sends more) do nothing. Any other frame starts the count over.
    pub fn on_frame(&mut self, frame: Frame) -> Option<u16> {
        if frame.throttle == self.last {
            self.repeats = self.repeats.saturating_add(1);
        } else {
            self.last = frame.throttle;
            self.repeats = 1;
        }
        if self.repeats != COMMAND_REPEATS {
            return None;
        }
        match frame.throttle {
            CMD_EDT_ENABLE => {
                self.enabled = true;
                Some(encode_edt_version(EDT_VERSION))
            }
            CMD_EDT_DISABLE => {
                self.enabled = false;
                Some(E12_EDT_DISABLED)
            }
            _ => None,
        }
    }
}

/// Encodes `e12` into the 21 line levels of the reply, left-aligned in a 32-bit word.
///
/// e12 → + inverted CRC → 4 nibbles → GCR (20 bits) → NRZI with a start bit (21 levels) → `<< 11`.
/// The PIO outputs bit 31 first; the lower 11 bits are zero.
pub fn encode_reply(e12: u16) -> u32 {
    let v = e12 & 0x0FFF;
    let word = (v << 4) | crc4_inverted(v);

    let mut gcr: u32 = 0;
    for i in (0..4).rev() {
        let nib = (word >> (i * 4)) & 0x0F;
        gcr = (gcr << 5) | GCR[nib as usize] as u32;
    }

    let mut level = 0u32; // start bit
    let mut out = level;
    for i in (0..20).rev() {
        level ^= (gcr >> i) & 1; // 1 = level change
        out = (out << 1) | level;
    }
    out << REPLY_SHIFT
}

/// Reasons [`decode_reply`] rejects a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyError {
    /// The first level is high: no start bit.
    NoStartBit,
    /// A 5-bit group is not a valid GCR code.
    InvalidGcr,
    /// The CRC does not match.
    BadCrc,
}

/// Decodes a reply word produced by [`encode_reply`] back into `e12`, as the FC does.
///
/// Only bits 31..11 are used.
pub fn decode_reply(word: u32) -> Result<u16, ReplyError> {
    let levels = word >> REPLY_SHIFT;
    if levels >> (REPLY_BITS - 1) != 0 {
        return Err(ReplyError::NoStartBit);
    }

    // A GCR bit is 1 where the level changed from the previous one.
    let gcr = (levels ^ (levels >> 1)) & 0xF_FFFF;

    let mut word16: u16 = 0;
    for i in (0..4).rev() {
        let nib = GCR_INV[((gcr >> (i * 5)) & 0x1F) as usize].ok_or(ReplyError::InvalidGcr)?;
        word16 = (word16 << 4) | nib as u16;
    }

    let e12 = word16 >> 4;
    if crc4_inverted(e12) != word16 & 0x0F {
        return Err(ReplyError::BadCrc);
    }
    Ok(e12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_matches_waveform() {
        // 0x5F4 → 0x5F41 → GCR 10101 01111 11101 11011
        // → levels 0 11001101010100101101, left-aligned
        assert_eq!(encode_reply(0x5F4), 0x66A9_6800);
    }

    #[test]
    fn frame_crc() {
        assert!(frame_ok(0x7D05));
        assert!(!frame_ok(0x7D04));
    }

    #[test]
    fn frame_crc_accepts_all_valid_frames_only() {
        for v in 0..0x1000u16 {
            let good = (v << 4) | crc4_inverted(v);
            for crc in 0..16 {
                assert_eq!(frame_ok((v << 4) | crc), (v << 4) | crc == good);
            }
        }
    }

    #[test]
    fn frame_example_from_plan() {
        // docs/bidirectional-dshot-plan.md, steps 2 and 4: throttle 1000, T = 0
        // → frame 0x7D05, read by the PIO as 0x82FA.
        let f = Frame {
            throttle: 1000,
            telemetry: false,
        };
        assert_eq!(encode_frame(f), 0x7D05);
        assert_eq!(decode_frame(0x82FA), Ok(f));
    }

    #[test]
    fn frame_telemetry_bit() {
        // Throttle 0, T = 1: v = 0b0000_0000_0001, CRC = !(1) & 0xF = 0xE.
        let f = Frame {
            throttle: 0,
            telemetry: true,
        };
        assert_eq!(encode_frame(f), 0x001E);
        assert_eq!(decode_frame(!0x001Eu32), Ok(f));
    }

    #[test]
    fn frame_roundtrip_all_values() {
        for throttle in 0..=THROTTLE_MAX {
            for telemetry in [false, true] {
                let f = Frame {
                    throttle,
                    telemetry,
                };
                let frame = encode_frame(f);
                assert!(frame_ok(frame));
                // The PIO delivers the line levels: inverted bits in the lower 16 bits.
                let raw = !frame as u32;
                assert_eq!(decode_frame(raw), Ok(f), "frame {frame:#06x}");
            }
        }
    }

    #[test]
    fn frame_single_bit_error_is_detected() {
        // Each data bit lands in exactly one nibble of the XOR, so flipping any single
        // bit of the frame (data or CRC) always breaks the CRC.
        for throttle in 0..=THROTTLE_MAX {
            for telemetry in [false, true] {
                let frame = encode_frame(Frame {
                    throttle,
                    telemetry,
                });
                for bit in 0..16 {
                    let raw = !(frame ^ (1 << bit)) as u32;
                    assert_eq!(decode_frame(raw), Err(FrameError::BadCrc));
                }
            }
        }
    }

    #[test]
    fn frame_command_range() {
        let f = |throttle| Frame {
            throttle,
            telemetry: false,
        };
        assert!(f(0).is_command());
        assert!(f(47).is_command());
        assert!(!f(48).is_command());
        assert!(!f(2047).is_command());
    }

    #[test]
    fn period_examples() {
        assert_eq!(encode_period(2000), 0x5F4); // 30 000 eRPM
        assert_eq!(encode_period(6000), 0x977); // 10 000 eRPM
        assert_eq!(encode_period(600), 0x32C); // 100 000 eRPM
        assert_eq!(encode_period(1333), 0x54D); // 45 000 eRPM, rounded down to 1332 µs
        assert_eq!(decode_period(0x54D), 1332);
    }

    #[test]
    fn period_lower_limit() {
        // docs/erpm-lower-limit.md
        assert_eq!(encode_period(65_280), 0xFFE); // ≈ 919 eRPM, slowest reportable
        assert_eq!(encode_period(65_407), 0xFFE);
        assert_eq!(encode_period(65_408), E12_STOPPED); // m = 511, e = 7
        assert_eq!(encode_period(65_535), E12_STOPPED);
        assert_eq!(encode_period(65_536), E12_STOPPED); // e > 7
        assert_eq!(encode_period(u32::MAX), E12_STOPPED);
        assert_eq!(encode_period(0), E12_STOPPED);
    }

    /// encode_period halves the period until it fits into 9 bits (≤ 511).
    /// Each halving throws away the lowest bit, so the FC may get back
    /// a slightly smaller period. This test checks that "slightly" is really small.
    ///
    /// | period | halvings                   | FC gets | lost, µs       |
    /// |--------|----------------------------|---------|----------------|
    /// |    500 | none, fits as is           |     500 | 0              |
    /// |   2000 | 1000 → 500                 |    2000 | 0 (lost bits were 0) |
    /// |  32895 | 16447 → … → 513 → 256 (×7) |   32768 | 127 µs ≈ 0.39 %, the worst case |
    #[test]
    fn period_roundtrip_error_is_small() {
        // Every period that is sent as real revolutions. 0 and 65 408 µs and up
        // are sent as "stopped" (0xFFF), see docs/erpm-lower-limit.md.
        for p in 1..=65_407u32 {
            let back = decode_period(encode_period(p));

            // Bits are only thrown away, never added: the result is never bigger.
            assert!(back <= p);

            // Lost no more than 1/256 of the period (0.4 %).
            // Written as `lost * 256 <= p` instead of `lost / p <= 1/256` to stay in integers.
            // For 32895: 127 * 256 = 32512 <= 32895.
            assert!((p - back) * 256 <= p, "period {p} decoded as {back}");
        }
    }

    #[test]
    fn edt_examples() {
        assert_eq!(encode_temperature(50), 0x232);
        assert_eq!(encode_current(17), 0x611);
        assert_eq!(decode_telemetry(0x232), Telemetry::Temperature(50));
        assert_eq!(decode_telemetry(0x611), Telemetry::Current(17));
        // Voltage (type 0x4, 0.25 V steps): 16.75 V = 67 steps, 15.25 V = 61 steps.
        assert_eq!(encode_voltage(67), 0x443);
        assert_eq!(encode_voltage(61), 0x43D);
        assert_eq!(decode_telemetry(0x443), Telemetry::Voltage(67));
        assert_eq!(encode_debug1(5), 0x805);
        assert_eq!(encode_debug2(200), 0xAC8);
        assert_eq!(decode_telemetry(0x805), Telemetry::Debug1(5));
        assert_eq!(decode_telemetry(0xAC8), Telemetry::Debug2(200));
        // Stress level (type 0xC) is not sent by us: stays Other.
        assert_eq!(
            decode_telemetry(0xC07),
            Telemetry::Other {
                kind: 0xC,
                value: 7
            }
        );
    }

    #[test]
    fn edt_status_examples() {
        // docs/day-005.md, table 2.1: warning at 70 °C with stress 11,
        // warning + alert at 84 °C with stress 14.
        let warning = Status {
            warning: true,
            max_stress: 11,
            ..Status::default()
        };
        let alert = Status {
            alert: true,
            warning: true,
            max_stress: 14,
            ..Status::default()
        };
        let error = Status {
            error: true,
            ..Status::default()
        };
        assert_eq!(encode_status(warning), 0xE4B);
        assert_eq!(encode_status(alert), 0xECE);
        assert_eq!(encode_status(error), 0xE20);
        assert_eq!(encode_status(Status::default()), 0xE00);
        assert_eq!(decode_telemetry(0xE4B), Telemetry::Status(warning));
        assert_eq!(decode_telemetry(0xECE), Telemetry::Status(alert));
        assert_eq!(decode_telemetry(0xE20), Telemetry::Status(error));
    }

    #[test]
    fn edt_status_roundtrip_all_values() {
        for flags in 0..8u8 {
            for max_stress in 0..=15u8 {
                let st = Status {
                    alert: flags & 4 != 0,
                    warning: flags & 2 != 0,
                    error: flags & 1 != 0,
                    max_stress,
                };
                let e12 = encode_status(st);
                assert_eq!(decode_telemetry(e12), Telemetry::Status(st), "{st:?}");
                assert_eq!(decode_reply(encode_reply(e12)), Ok(e12));
            }
        }
    }

    #[test]
    fn edt_roundtrip_all_values() {
        for v in 0..=u8::MAX {
            assert_eq!(
                decode_telemetry(encode_temperature(v)),
                Telemetry::Temperature(v)
            );
            assert_eq!(decode_telemetry(encode_voltage(v)), Telemetry::Voltage(v));
            assert_eq!(decode_telemetry(encode_current(v)), Telemetry::Current(v));
            assert_eq!(decode_telemetry(encode_debug1(v)), Telemetry::Debug1(v));
            assert_eq!(decode_telemetry(encode_debug2(v)), Telemetry::Debug2(v));
            // Through the line as well: CRC, GCR and NRZI do not care what e12 means.
            let e12 = encode_temperature(v);
            assert_eq!(decode_reply(encode_reply(e12)), Ok(e12));
        }
    }

    fn feed(edt: &mut EdtSwitch, throttle: u16, times: usize) -> Vec<Option<u16>> {
        let frame = Frame {
            throttle,
            telemetry: true,
        };
        (0..times).map(|_| edt.on_frame(frame)).collect()
    }

    #[test]
    fn edt_switch_needs_six_in_a_row() {
        let mut edt = EdtSwitch::default();
        assert!(!edt.enabled());

        // Five times is not enough.
        assert_eq!(feed(&mut edt, CMD_EDT_ENABLE, 5), [None; 5]);
        assert!(!edt.enabled());
        // The sixth turns EDT on and asks for the version frame once.
        assert_eq!(
            edt.on_frame(Frame {
                throttle: CMD_EDT_ENABLE,
                telemetry: true
            }),
            Some(0xE02)
        );
        assert!(edt.enabled());
        // Betaflight keeps repeating: nothing more happens.
        assert_eq!(feed(&mut edt, CMD_EDT_ENABLE, 10), [None; 10]);
        assert!(edt.enabled());

        // Throttle frames in between do not change anything.
        assert_eq!(feed(&mut edt, 1000, 100), [None; 100]);
        assert!(edt.enabled());

        // Off: the sixth "14" in a row, answered with 0xEFF.
        let replies = feed(&mut edt, CMD_EDT_DISABLE, 6);
        assert_eq!(replies[..5], [None; 5]);
        assert_eq!(replies[5], Some(E12_EDT_DISABLED));
        assert!(!edt.enabled());
    }

    #[test]
    fn edt_switch_enabled_at_start() {
        let mut edt = EdtSwitch::enabled_at_start();
        assert!(edt.enabled());
        // No version frame is sent on its own; command 13 later still answers once.
        assert_eq!(feed(&mut edt, 1000, 100), [None; 100]);
        assert_eq!(feed(&mut edt, CMD_EDT_ENABLE, 6)[5], Some(0xE02));
        assert!(edt.enabled());
        // Command 14 turns it off.
        assert_eq!(
            feed(&mut edt, CMD_EDT_DISABLE, 6)[5],
            Some(E12_EDT_DISABLED)
        );
        assert!(!edt.enabled());
    }

    #[test]
    fn edt_switch_count_restarts_on_other_frame() {
        let mut edt = EdtSwitch::default();
        feed(&mut edt, CMD_EDT_ENABLE, 5);
        feed(&mut edt, 0, 1); // "motor stop" in between
        assert_eq!(feed(&mut edt, CMD_EDT_ENABLE, 5), [None; 5]);
        assert!(!edt.enabled());
        assert_eq!(feed(&mut edt, CMD_EDT_ENABLE, 1), [Some(0xE02)]);
        assert!(edt.enabled());
    }

    #[test]
    fn edt_control_frames_are_edt() {
        // The version frame and "EDT off" share type 0xE with Status: without the
        // context of command 13/14 they decode as a status, never as a period.
        assert_eq!(
            decode_telemetry(encode_edt_version(EDT_VERSION)),
            Telemetry::Status(Status {
                max_stress: EDT_VERSION,
                ..Status::default()
            })
        );
        assert_eq!(
            decode_telemetry(E12_EDT_DISABLED),
            Telemetry::Status(Status {
                alert: true,
                warning: true,
                error: true,
                max_stress: 15
            })
        );
    }

    #[test]
    fn period_is_never_taken_for_edt() {
        // Every e12 that encode_period can produce, including "stopped", must read back
        // as a period, otherwise the FC would show a made-up temperature or current.
        for p in 0..=65_536u32 {
            let e12 = encode_period(p);
            assert_eq!(
                decode_telemetry(e12),
                Telemetry::Period(decode_period(e12)),
                "period {p}"
            );
        }
        assert_eq!(decode_telemetry(E12_STOPPED), Telemetry::Period(65_408));
    }

    #[test]
    fn reply_roundtrip_all_values() {
        for v in 0..0x1000u16 {
            assert_eq!(decode_reply(encode_reply(v)), Ok(v), "e12 = {v:#05x}");
        }
    }

    #[test]
    fn reply_shape() {
        for v in 0..0x1000u16 {
            let w = encode_reply(v);
            assert_eq!(w >> 31, 0, "start bit must pull the line low");
            assert_eq!(w & ((1 << REPLY_SHIFT) - 1), 0, "lower 11 bits unused");

            // GCR guarantees an edge at least every 3 bits.
            let levels = w >> REPLY_SHIFT;
            let mut run = 1;
            for i in (0..REPLY_BITS - 1).rev() {
                if (levels >> i) & 1 == (levels >> (i + 1)) & 1 {
                    run += 1;
                    assert!(run <= 3, "e12 = {v:#05x}: {run} equal levels in a row");
                } else {
                    run = 1;
                }
            }
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        // Empty TX FIFO: PIO sends X = 0xFFFFFFFF, the line stays high.
        assert_eq!(decode_reply(0xFFFF_FFFF), Err(ReplyError::NoStartBit));
        // Line held low: GCR 00000 is not a valid code.
        assert_eq!(decode_reply(0), Err(ReplyError::InvalidGcr));
        // Valid GCR, wrong CRC: 0x5F4 with CRC 0x0 instead of 0x1.
        let mut gcr: u32 = 0;
        for nib in [0x5, 0xF, 0x4, 0x0] {
            gcr = (gcr << 5) | GCR[nib] as u32;
        }
        let (mut level, mut levels) = (0u32, 0u32);
        for i in (0..20).rev() {
            level ^= (gcr >> i) & 1;
            levels = (levels << 1) | level;
        }
        assert_eq!(decode_reply(levels << REPLY_SHIFT), Err(ReplyError::BadCrc));
    }
}
