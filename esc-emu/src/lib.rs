//! Bench ESC emulation: what the RP2040 answers to each DShot frame from the FC.
//!
//! The firmware feeds every value from the PIO RX FIFO into [`Esc::on_raw`] and, once
//! per wake-up of its task, pushes [`Esc::next_reply`] into the TX FIFO. Everything
//! that does not touch the hardware lives here, so it can be tested on the PC.
//!
//! - eRPM model: [`ERPM_BASE`] + throttle × [`ERPM_PER_STEP`], see [`erpm_for`].
//! - EDT: every [`EDT_EVERY`]-th reply carries one EDT value instead of the period,
//!   in turn: temperature, voltage, current, status, debug 1, debug 2
//!   (see `docs/edt.md`, `docs/day-005.md`).
//!
//! The numbers are a bench toy, not a motor model: there is no motor.
#![cfg_attr(not(test), no_std)]

use dshot_codec::{
    decode_frame, encode_current, encode_debug1, encode_debug2, encode_period, encode_reply,
    encode_status, encode_temperature, encode_voltage, EdtSwitch, Frame, FrameError, Status,
    E12_STOPPED, THROTTLE_MAX, THROTTLE_MIN,
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

/// With EDT on, every `EDT_EVERY`-th reply carries an EDT value instead of the period:
/// 32 × 250 µs = 8 ms between EDT values, so each of the six types comes every 48 ms,
/// and the FC loses 1/32 of the eRPM samples (≈ 3 %).
pub const EDT_EVERY: u32 = 32;

/// eRPM at which temperature and voltage reach their full-throttle values.
pub const ERPM_FULL: u32 = 42_000;

/// Temperature at 0 eRPM, °C; grows linearly to [`TEMP_FULL_C`] at [`ERPM_FULL`].
pub const TEMP_IDLE_C: u32 = 25;
/// Temperature at [`ERPM_FULL`], °C.
pub const TEMP_FULL_C: u32 = 85;
/// Status warning from this temperature on, °C.
pub const TEMP_WARNING_C: u8 = 70;
/// Status alert from this temperature on, °C.
pub const TEMP_ALERT_C: u8 = 80;

/// Voltage at 0 eRPM, mV (a full 4S battery); sags linearly to [`VOLT_FULL_MV`]
/// at [`ERPM_FULL`].
pub const VOLT_IDLE_MV: u32 = 16_800;
/// Voltage at [`ERPM_FULL`], mV.
pub const VOLT_FULL_MV: u32 = 14_000;
/// One EDT voltage step, mV.
pub const VOLT_STEP_MV: u32 = 250;

/// EDT current at full throttle, A: grows linearly from 0 A at throttle 48.
pub const EDT_CURRENT_MAX_A: u32 = 20;

/// eRPM of the model: 0 for special commands (throttle 0–47, "motor stopped").
pub fn erpm_for(frame: Frame) -> u32 {
    if frame.is_command() {
        return 0;
    }
    ERPM_BASE + frame.throttle as u32 * ERPM_PER_STEP
}

/// The period reply for a frame: "motor stopped" for special commands,
/// otherwise the eRPM of the model.
pub fn reply_for(frame: Frame) -> u32 {
    match erpm_for(frame) {
        0 => encode_reply(E12_STOPPED),
        erpm => encode_reply(encode_period(US_PER_MINUTE / erpm)),
    }
}

/// EDT temperature, °C: 25 °C at 0 eRPM to 85 °C at 42 000 eRPM, fraction dropped.
pub fn temperature_for(frame: Frame) -> u8 {
    let rise = (TEMP_FULL_C - TEMP_IDLE_C) * erpm_for(frame) / ERPM_FULL;
    (TEMP_IDLE_C + rise) as u8
}

/// EDT voltage in 0.25 V steps: 16.8 V at 0 eRPM sagging to 14.0 V at 42 000 eRPM,
/// fraction dropped (16.8 V → 67 steps = 16.75 V).
pub fn voltage_for(frame: Frame) -> u8 {
    let sag = (VOLT_IDLE_MV - VOLT_FULL_MV) * erpm_for(frame) / ERPM_FULL;
    ((VOLT_IDLE_MV - sag) / VOLT_STEP_MV) as u8
}

/// EDT current, A: 0 A for special commands, otherwise 0 A at throttle 48 to 20 A
/// at 2047, fraction dropped.
pub fn current_for(frame: Frame) -> u8 {
    if frame.is_command() {
        return 0;
    }
    let step = (frame.throttle - THROTTLE_MIN) as u32;
    (step * EDT_CURRENT_MAX_A / (THROTTLE_MAX - THROTTLE_MIN) as u32) as u8
}

/// EDT status from the temperature: warning from 70 °C, alert from 80 °C, never error;
/// max stress 0–15 grows linearly with the temperature above 25 °C.
pub fn status_for(frame: Frame) -> Status {
    let t = temperature_for(frame);
    Status {
        alert: t >= TEMP_ALERT_C,
        warning: t >= TEMP_WARNING_C,
        error: false,
        max_stress: ((t as u32 - TEMP_IDLE_C) * 15 / (TEMP_FULL_C - TEMP_IDLE_C)) as u8,
    }
}

/// Firmware counters sent as EDT debug values; the ESC knows nothing about USB
/// or time, so the firmware passes them in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Frames with a bad CRC; the low byte goes out as debug 1.
    pub bad: u32,
    /// Late wake-ups of the PIO task; the low byte goes out as debug 2.
    pub late: u32,
}

/// The EDT values in the order they go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdtKind {
    Temperature,
    Voltage,
    Current,
    Status,
    Debug1,
    Debug2,
}

const EDT_ORDER: [EdtKind; 6] = [
    EdtKind::Temperature,
    EdtKind::Voltage,
    EdtKind::Current,
    EdtKind::Status,
    EdtKind::Debug1,
    EdtKind::Debug2,
];

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
    /// Index into [`EDT_ORDER`] of the next EDT value.
    edt_next: usize,
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
            edt_next: 0,
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
    /// `counters` go out as debug 1 and debug 2 when it is their turn.
    pub fn next_reply(&mut self, counters: Counters) -> u32 {
        // The EDT version frame (or "EDT off") goes out once, instead of the period.
        if let Some(e12) = self.one_off.take() {
            return encode_reply(e12);
        }
        if !self.edt.enabled() {
            return self.reply;
        }
        // With EDT on, every EDT_EVERY-th reply is the next EDT value instead of the period.
        self.since_edt += 1;
        if self.since_edt < EDT_EVERY {
            return self.reply;
        }
        self.since_edt = 0;
        let kind = EDT_ORDER[self.edt_next];
        self.edt_next = (self.edt_next + 1) % EDT_ORDER.len();
        encode_reply(self.edt_value(kind, counters))
    }

    /// Whether EDT is on.
    pub fn edt_enabled(&self) -> bool {
        self.edt.enabled()
    }

    /// `e12` of one EDT value, from the last frame with a good CRC.
    /// Before the first frame the motor counts as stopped.
    fn edt_value(&self, kind: EdtKind, counters: Counters) -> u16 {
        let frame = self.last_frame.unwrap_or(Frame {
            throttle: 0,
            telemetry: false,
        });
        match kind {
            EdtKind::Temperature => encode_temperature(temperature_for(frame)),
            EdtKind::Voltage => encode_voltage(voltage_for(frame)),
            EdtKind::Current => encode_current(current_for(frame)),
            EdtKind::Status => encode_status(status_for(frame)),
            EdtKind::Debug1 => encode_debug1(counters.bad as u8),
            EdtKind::Debug2 => encode_debug2(counters.late as u8),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dshot_codec::{decode_reply, decode_telemetry, encode_frame, Telemetry, CMD_EDT_ENABLE};

    fn frame(throttle: u16) -> Frame {
        Frame {
            throttle,
            telemetry: false,
        }
    }

    /// The value the PIO puts into the RX FIFO for a frame from the FC (inverted bits).
    fn raw(throttle: u16) -> u32 {
        !encode_frame(frame(throttle)) as u32
    }

    /// What the FC makes of a reply word.
    fn fc(word: u32) -> Telemetry {
        decode_telemetry(decode_reply(word).expect("reply must decode"))
    }

    /// One wake-up of the task with a single frame; returns the reply it queues.
    fn wake(esc: &mut Esc, throttle: u16, counters: Counters) -> u32 {
        esc.on_raw(raw(throttle));
        esc.next_reply(counters)
    }

    fn period(throttle: u32) -> Telemetry {
        let e12 = encode_period(US_PER_MINUTE / (ERPM_BASE + throttle * ERPM_PER_STEP));
        decode_telemetry(e12)
    }

    fn status(alert: bool, warning: bool, max_stress: u8) -> Status {
        Status {
            alert,
            warning,
            error: false,
            max_stress,
        }
    }

    #[test]
    fn erpm_model_by_throttle() {
        // Same throttles as the bench sweep (reports/004-with-probe-rs/hardware-in-the-loop.md).
        for t in [48u16, 546, 1046, 1546, 2047] {
            assert_eq!(fc(reply_for(frame(t))), period(t as u32), "throttle {t}");
        }
        // e.g. throttle 1000: 21 000 eRPM → 2857 µs → e12 0x765 (docs/action-plan.md, stage C).
        assert_eq!(decode_reply(reply_for(frame(1000))), Ok(0x765));
    }

    #[test]
    fn commands_mean_motor_stopped() {
        for t in [0u16, 13, 47] {
            assert_eq!(erpm_for(frame(t)), 0);
            assert_eq!(decode_reply(reply_for(frame(t))), Ok(E12_STOPPED));
            assert_eq!(current_for(frame(t)), 0);
        }
    }

    /// docs/day-005.md, table 2.1.
    #[test]
    fn edt_models_table() {
        let table = [
            // throttle, °C, 0.25 V steps, A, status
            (0u16, 25u8, 67u8, 0u8, status(false, false, 0)),
            (48, 27, 66, 0, status(false, false, 0)),
            (546, 42, 64, 4, status(false, false, 4)),
            (1046, 56, 61, 9, status(false, false, 7)),
            (1546, 70, 58, 14, status(false, true, 11)),
            (2047, 84, 56, 20, status(true, true, 14)),
        ];
        for (t, celsius, steps, amps, st) in table {
            let f = frame(t);
            assert_eq!(temperature_for(f), celsius, "temperature, throttle {t}");
            assert_eq!(voltage_for(f), steps, "voltage, throttle {t}");
            assert_eq!(current_for(f), amps, "current, throttle {t}");
            assert_eq!(status_for(f), st, "status, throttle {t}");
        }
        // The status frames from the plan: warning + stress 11, warning + alert + stress 14.
        assert_eq!(encode_status(status_for(frame(1546))), 0xE4B);
        assert_eq!(encode_status(status_for(frame(2047))), 0xECE);
    }

    #[test]
    fn first_reply_is_motor_stopped() {
        let esc = Esc::new(true);
        assert_eq!(decode_reply(esc.first_reply()), Ok(E12_STOPPED));
    }

    #[test]
    fn edt_every_32nd_reply_in_turn() {
        let mut esc = Esc::new(true);
        // Only the low byte goes out: 0x103 → 0x03, 0x202 → 0x02.
        let counters = Counters {
            bad: 0x103,
            late: 0x202,
        };
        let mut edt = Vec::new();
        for n in 1..=32 * 7 {
            match fc(wake(&mut esc, 1546, counters)) {
                Telemetry::Period(_) => {}
                t => edt.push((n, t)),
            }
        }
        assert_eq!(
            edt,
            [
                (32, Telemetry::Temperature(70)),
                (64, Telemetry::Voltage(58)),
                (96, Telemetry::Current(14)),
                (128, Telemetry::Status(status(false, true, 11))),
                (160, Telemetry::Debug1(0x03)),
                (192, Telemetry::Debug2(0x02)),
                (224, Telemetry::Temperature(70)), // and round again
            ]
        );
    }

    #[test]
    fn edt_before_first_frame_counts_as_stopped() {
        let mut esc = Esc::new(true);
        let replies: Vec<_> = (0..32)
            .map(|_| fc(esc.next_reply(Counters::default())))
            .collect();
        assert_eq!(replies[31], Telemetry::Temperature(25));
    }

    #[test]
    fn bad_crc_keeps_last_good_reply() {
        let mut esc = Esc::new(false);
        let good = wake(&mut esc, 1000, Counters::default());
        // Flip one bit: the CRC no longer matches.
        let rx = esc.on_raw(raw(1500) ^ 1);
        assert_eq!(rx.result, Err(FrameError::BadCrc));
        assert_eq!(esc.next_reply(Counters::default()), good);
    }

    #[test]
    fn late_wake_up_answers_newest_frame() {
        let mut esc = Esc::new(false);
        for t in [100u16, 500, 1000] {
            esc.on_raw(raw(t));
        }
        assert_eq!(fc(esc.next_reply(Counters::default())), period(1000));
    }

    #[test]
    fn edt_waits_for_command_13_when_not_at_start() {
        let mut esc = Esc::new(false);
        let c = Counters::default();
        // No EDT without the command.
        for _ in 0..200 {
            assert!(matches!(fc(wake(&mut esc, 1046, c)), Telemetry::Period(_)));
        }
        assert!(!esc.edt_enabled());
        // Six commands 13 in a row: the sixth turns EDT on and the version frame goes out once.
        let mut events = Vec::new();
        for _ in 0..6 {
            let rx = esc.on_raw(raw(CMD_EDT_ENABLE));
            events.push(rx.edt_event);
            let reply = esc.next_reply(c);
            if rx.edt_event.is_some() {
                assert_eq!(decode_reply(reply), Ok(0xE02));
            }
        }
        assert_eq!(events, [None, None, None, None, None, Some(0xE02)]);
        assert!(esc.edt_enabled());
        // Then EDT values come every 32nd reply, starting with the temperature.
        let first_edt =
            (1..=32).find(|_| !matches!(fc(wake(&mut esc, 1046, c)), Telemetry::Period(_)));
        assert_eq!(first_edt, Some(32));
    }

    #[test]
    fn edt_at_start_needs_no_command() {
        let mut esc = Esc::new(true);
        assert!(esc.edt_enabled());
        let replies: Vec<_> = (0..32)
            .map(|_| fc(wake(&mut esc, 1046, Counters::default())))
            .collect();
        // No version frame; the first EDT value is the temperature, on the 32nd reply.
        assert!(replies[..31]
            .iter()
            .all(|t| matches!(t, Telemetry::Period(_))));
        assert_eq!(replies[31], Telemetry::Temperature(56));
    }
}
