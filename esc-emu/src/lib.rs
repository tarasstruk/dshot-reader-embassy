//! Bench ESC emulation: what the RP2040 answers to each DShot frame from the FC.
//!
//! The firmware feeds every value from the PIO RX FIFO into [`Esc::on_raw`] and, once
//! per wake-up of its task, pushes [`Esc::next_reply`] into the TX FIFO. Everything
//! that does not touch the hardware lives here, so it can be tested on the PC.
//!
//! - eRPM model: [`ERPM_BASE`] + throttle × [`ERPM_PER_STEP`], see [`reply_for`].
//! - EDT: every [`EDT_EVERY`]-th reply carries the temperature or the current, in turn,
//!   see `docs/edt.md`.
//!
//! The numbers are a bench toy, not a motor model: there is no motor.
#![cfg_attr(not(test), no_std)]

use dshot_codec::{
    decode_frame, encode_current, encode_period, encode_reply, encode_temperature, EdtSwitch,
    Frame, FrameError, E12_STOPPED, THROTTLE_MAX, THROTTLE_MIN,
};

/// eRPM model: the reply eRPM grows linearly with throttle,
/// `ERPM_BASE + throttle × ERPM_PER_STEP` (docs/bidirectional-dshot-plan.md, step 8):
/// throttle 48 → 1 960 eRPM, 1000 → 21 000, 2047 → 41 940.
///
/// The base keeps the slowest reply above ≈ 919 eRPM, the lowest the reply can carry
/// (docs/erpm-lower-limit.md); the step makes full throttle ≈ 42 000 eRPM, i.e.
/// ≈ 6 000 RPM with 7 pole pairs.
pub const ERPM_BASE: u32 = 1_000;
/// See [`ERPM_BASE`].
pub const ERPM_PER_STEP: u32 = 20;

/// The reply carries the period of one electrical revolution in µs, not eRPM:
/// period = 60 s × 1 000 000 µs/s / eRPM, so `period_us = US_PER_MINUTE / erpm`.
/// E.g. 30 000 eRPM → 60 000 000 / 30 000 = 2000 µs.
pub const US_PER_MINUTE: u32 = 60_000_000;

/// With EDT on, every `EDT_EVERY`-th reply carries temperature or current (in turn)
/// instead of the period: 64 × 250 µs = 16 ms, so each value ≈ every 32 ms, and the FC
/// loses only 1/64 of the eRPM samples. See docs/edt.md.
pub const EDT_EVERY: u32 = 64;

/// EDT temperature, °C: fixed, there is no motor to heat up.
pub const EDT_TEMPERATURE_C: u8 = 42;
/// EDT current at full throttle, A: grows linearly from 0 A at throttle 48.
pub const EDT_CURRENT_MAX_A: u32 = 20;

/// The reply for a frame: "motor stopped" for special commands (throttle 0–47),
/// otherwise the eRPM of the linear model.
pub fn reply_for(frame: Frame) -> u32 {
    if frame.is_command() {
        return encode_reply(E12_STOPPED);
    }
    let erpm = ERPM_BASE + frame.throttle as u32 * ERPM_PER_STEP;
    encode_reply(encode_period(US_PER_MINUTE / erpm))
}

/// The EDT current for a frame: 0 A for special commands, otherwise the linear model.
pub fn current_for(frame: Frame) -> u8 {
    if frame.is_command() {
        return 0;
    }
    let step = (frame.throttle - THROTTLE_MIN) as u32;
    (step * EDT_CURRENT_MAX_A / (THROTTLE_MAX - THROTTLE_MIN) as u32) as u8
}

/// What [`Esc::on_raw`] made of one value from the RX FIFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// The decoded frame, or why it was rejected.
    pub result: Result<Frame, FrameError>,
    /// `e12` of a one-off EDT reply when DShot command 13 or 14 just took effect
    /// (version frame or "EDT off"); the firmware logs it.
    pub edt_event: Option<u16>,
}

/// The emulated ESC: keeps the reply one frame ahead, as the PIO needs it.
///
/// The reply has to be in the TX FIFO before the frame arrives: the PIO takes it
/// ~30 µs after the frame, too soon for the CPU. So the reply computed after frame N
/// goes out with frame N + 1, as real ESCs answer with the last measured value.
#[derive(Debug, Clone)]
pub struct Esc {
    edt: EdtSwitch,
    /// Period reply for the last frame with a good CRC.
    reply: u32,
    /// Last frame with a good CRC.
    last_frame: Option<Frame>,
    /// Version frame or "EDT off", to go out once instead of the period.
    one_off: Option<u16>,
    /// Replies since the last EDT value.
    since_edt: u32,
    /// EDT values go in turn: temperature, current, ...
    current_next: bool,
}

impl Esc {
    /// A new ESC. With `edt_at_start` EDT is on from the first reply, without
    /// DShot command 13 (for Betaflight `dshot_edt = FORCE`, docs/edt.md).
    pub fn new(edt_at_start: bool) -> Self {
        Esc {
            edt: if edt_at_start {
                EdtSwitch::enabled_at_start()
            } else {
                EdtSwitch::default()
            },
            reply: encode_reply(E12_STOPPED),
            last_frame: None,
            one_off: None,
            since_edt: 0,
            current_next: false,
        }
    }

    /// The reply to queue before the first frame: "motor stopped".
    pub fn first_reply(&self) -> u32 {
        encode_reply(E12_STOPPED)
    }

    /// Takes one value from the RX FIFO (line levels, i.e. inverted bits).
    ///
    /// A frame with a good CRC updates the reply; after a bad one the reply for the
    /// last good frame is kept. When the task was late, call this for every waiting
    /// value: only the newest frame decides the reply (docs/rx-overflow-plan.md).
    pub fn on_raw(&mut self, raw: u32) -> Received {
        let result = decode_frame(raw);
        let mut edt_event = None;
        if let Ok(frame) = result {
            // DShot commands 13/14, six times in a row, turn EDT on/off.
            if let Some(e12) = self.edt.on_frame(frame) {
                self.one_off = Some(e12);
                edt_event = Some(e12);
            }
            self.reply = reply_for(frame);
            self.last_frame = Some(frame);
        }
        Received { result, edt_event }
    }

    /// The reply to push into the TX FIFO, once per wake-up of the task.
    pub fn next_reply(&mut self) -> u32 {
        // The EDT version frame (or "EDT off") goes out once, instead of the period.
        if let Some(e12) = self.one_off.take() {
            return encode_reply(e12);
        }
        if !self.edt.enabled() {
            return self.reply;
        }
        // With EDT on, now and then temperature or current instead of the period.
        self.since_edt += 1;
        if self.since_edt < EDT_EVERY {
            return self.reply;
        }
        self.since_edt = 0;
        let e12 = if self.current_next {
            encode_current(self.last_frame.map_or(0, current_for))
        } else {
            encode_temperature(EDT_TEMPERATURE_C)
        };
        self.current_next = !self.current_next;
        encode_reply(e12)
    }

    /// Whether EDT is on.
    pub fn edt_enabled(&self) -> bool {
        self.edt.enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshot_codec::{decode_reply, decode_telemetry, encode_frame, Telemetry, CMD_EDT_ENABLE};

    /// The value the PIO puts into the RX FIFO for a frame from the FC (inverted bits).
    fn raw(throttle: u16) -> u32 {
        !encode_frame(Frame {
            throttle,
            telemetry: false,
        }) as u32
    }

    /// What the FC makes of a reply word.
    fn fc(word: u32) -> Telemetry {
        decode_telemetry(decode_reply(word).expect("reply must decode"))
    }

    /// One wake-up of the task with a single frame; returns the reply it queues.
    fn wake(esc: &mut Esc, throttle: u16) -> u32 {
        esc.on_raw(raw(throttle));
        esc.next_reply()
    }

    fn period(throttle: u32) -> Telemetry {
        let e12 = encode_period(US_PER_MINUTE / (ERPM_BASE + throttle * ERPM_PER_STEP));
        decode_telemetry(e12)
    }

    #[test]
    fn erpm_model_by_throttle() {
        // Same throttles as the bench sweep (reports/004-with-probe-rs/hardware-in-the-loop.md).
        for t in [48u16, 546, 1046, 1546, 2047] {
            let f = Frame {
                throttle: t,
                telemetry: false,
            };
            assert_eq!(fc(reply_for(f)), period(t as u32), "throttle {t}");
        }
        // e.g. throttle 1000: 21 000 eRPM → 2857 µs → e12 0x765 (docs/action-plan.md, stage C).
        let f = Frame {
            throttle: 1000,
            telemetry: false,
        };
        assert_eq!(decode_reply(reply_for(f)), Ok(0x765));
    }

    #[test]
    fn commands_mean_motor_stopped() {
        for t in [0u16, 13, 47] {
            let f = Frame {
                throttle: t,
                telemetry: false,
            };
            assert_eq!(decode_reply(reply_for(f)), Ok(E12_STOPPED));
            assert_eq!(current_for(f), 0);
        }
    }

    #[test]
    fn current_model() {
        // (throttle − 48) × 20 / 1999, fraction dropped (reports/004-with-probe-rs/edt-details.md).
        for (t, amps) in [(48u16, 0u8), (546, 4), (1046, 9), (1546, 14), (2047, 20)] {
            let f = Frame {
                throttle: t,
                telemetry: false,
            };
            assert_eq!(current_for(f), amps, "throttle {t}");
        }
    }

    #[test]
    fn first_reply_is_motor_stopped() {
        let esc = Esc::new(true);
        assert_eq!(decode_reply(esc.first_reply()), Ok(E12_STOPPED));
    }

    #[test]
    fn edt_every_64th_reply_in_turn() {
        let mut esc = Esc::new(true);
        let mut edt = Vec::new();
        for n in 1..=64 * 4 {
            match fc(wake(&mut esc, 1046)) {
                Telemetry::Period(_) => {}
                t => edt.push((n, t)),
            }
        }
        assert_eq!(
            edt,
            [
                (64, Telemetry::Temperature(EDT_TEMPERATURE_C)),
                (128, Telemetry::Current(9)),
                (192, Telemetry::Temperature(EDT_TEMPERATURE_C)),
                (256, Telemetry::Current(9)),
            ]
        );
    }

    #[test]
    fn bad_crc_keeps_last_good_reply() {
        let mut esc = Esc::new(false);
        let good = wake(&mut esc, 1000);
        // Flip one bit: the CRC no longer matches.
        let rx = esc.on_raw(raw(1500) ^ 1);
        assert_eq!(rx.result, Err(FrameError::BadCrc));
        assert_eq!(esc.next_reply(), good);
    }

    #[test]
    fn late_wake_up_answers_newest_frame() {
        let mut esc = Esc::new(false);
        for t in [100u16, 500, 1000] {
            esc.on_raw(raw(t));
        }
        assert_eq!(fc(esc.next_reply()), period(1000));
    }

    #[test]
    fn edt_waits_for_command_13_when_not_at_start() {
        let mut esc = Esc::new(false);
        // No EDT without the command.
        for _ in 0..200 {
            assert!(matches!(fc(wake(&mut esc, 1046)), Telemetry::Period(_)));
        }
        assert!(!esc.edt_enabled());
        // Six commands 13 in a row: the sixth turns EDT on and the version frame goes out once.
        let mut events = Vec::new();
        for _ in 0..6 {
            let rx = esc.on_raw(raw(CMD_EDT_ENABLE));
            events.push(rx.edt_event);
            let reply = esc.next_reply();
            if rx.edt_event.is_some() {
                assert_eq!(decode_reply(reply), Ok(0xE02));
            }
        }
        assert_eq!(events, [None, None, None, None, None, Some(0xE02)]);
        assert!(esc.edt_enabled());
        // Then EDT values come every 64th reply.
        let first_edt =
            (1..=64).find(|_| !matches!(fc(wake(&mut esc, 1046)), Telemetry::Period(_)));
        assert_eq!(first_edt, Some(64));
    }

    #[test]
    fn edt_at_start_needs_no_command() {
        let mut esc = Esc::new(true);
        assert!(esc.edt_enabled());
        let replies: Vec<_> = (0..64).map(|_| fc(wake(&mut esc, 1046))).collect();
        // No version frame; the first EDT value is the temperature, on the 64th reply.
        assert!(replies[..63]
            .iter()
            .all(|t| matches!(t, Telemetry::Period(_))));
        assert_eq!(replies[63], Telemetry::Temperature(EDT_TEMPERATURE_C));
    }
}
