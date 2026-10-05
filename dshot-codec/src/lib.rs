//! Hardware-independent encoding for bidirectional DShot.
//!
//! - [`decode_frame`] turns a value from the PIO RX FIFO into a [`Frame`] from the FC.
//! - [`encode_frame`] is the reverse of [`decode_frame`], as done by the FC.
//! - [`frame_ok`] checks the inverted CRC of a 16-bit frame received from the FC.
//! - [`encode_period`] packs an electrical revolution period into the 12-bit `e12` format.
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
