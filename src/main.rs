#![no_std]
#![no_main]

use {defmt_rtt as _, panic_probe as _};

use core::cell::Cell;
use core::fmt::Write as _;
use dshot_codec::{Frame, FrameError};
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_rp::clocks::clk_sys_freq;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{PIO0, USB};
use embassy_rp::pio::program::pio_asm;
use embassy_rp::pio::{
    Common, Config, Direction, Pio, PioPin, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_rp::usb::{Driver, Instance, InterruptHandler as UsbInterruptHandler};
use embassy_rp::Peri;
use embassy_rp::{bind_interrupts, pio::InterruptHandler as PioInterruptHandler};
use embassy_sync::blocking_mutex::{raw::ThreadModeRawMutex, Mutex};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, Sender, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config as UsbConfig};
use esc_emu::{Counters, Esc};
use fixed::types::extra::U8;

/// PIO state machine clock: 12 MHz gives whole numbers of ticks for both
/// the command bit (3.333 µs = 40 ticks) and the reply bit (2.667 µs = 32 ticks).
const PIO_CLOCK_HZ: u32 = 12_000_000;

/// Send EDT from the start, without waiting for DShot command 13. Betaflight sends
/// command 13 only when arming, and the bench FC cannot arm; with `dshot_edt = FORCE`
/// it decodes EDT without the command. With `dshot_edt = ON` set this to false.
/// See docs/edt.md.
const EDT_AT_START: bool = true;

/// How often the latest frame and counters are sent over USB.
const REPORT_PERIOD_MS: u64 = 500;

/// How often to look whether a terminal has opened the USB port (DTR set).
const DTR_POLL_MS: u64 = 10;

/// How long one USB packet may wait for the host. A host that keeps the port open but does
/// not read would otherwise stop the USB loop; after this the line is dropped, so nothing
/// piles up to come out later as a stale line.
const USB_WRITE_TIMEOUT_MS: u64 = 50;

/// A wake-up of the PIO task later than this after the previous one counts as late.
/// Frames come every 250 µs at 4 kHz, and the RX FIFO holds 4 of them,
/// so after ~1 ms the PIO starts dropping frames.
const LATE_GAP_US: u64 = 1_000;

/// What the PIO task has seen so far. Counted over every frame,
/// while USB only reports a snapshot every [`REPORT_PERIOD_MS`].
#[derive(Clone, Copy, Default)]
struct Stats {
    /// Latest value from the RX FIFO (line levels, i.e. inverted bits).
    last_raw: Option<u32>,
    /// Latest decoding result.
    last: Option<Result<Frame, FrameError>>,
    /// All frames received.
    frames: u32,
    /// Frames rejected because of a bad CRC.
    bad_crc: u32,
    /// Replies not queued because the TX FIFO was full.
    tx_full: u32,
    /// Wake-ups of the PIO task more than [`LATE_GAP_US`] after the previous one.
    late: u32,
    /// Longest time between two wake-ups since the last USB report, µs.
    max_gap_us: u32,
    /// Extended telemetry (EDT) turned on by the FC (DShot command 13).
    edt: bool,
}

static STATS: Mutex<ThreadModeRawMutex, Cell<Stats>> = Mutex::new(Cell::new(Stats {
    last_raw: None,
    last: None,
    frames: 0,
    bad_crc: 0,
    tx_full: 0,
    late: 0,
    max_gap_us: 0,
    edt: false,
}));

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
});

// Setup PIO SM0: receive inverted (bidirectional) DShot300 frames and reply with telemetry.
fn setup_pio_task_sm0<'d>(
    pio: &mut Common<'d, PIO0>,
    sm: &mut StateMachine<'d, PIO0, 0>,
    dshot_pin: Peri<'d, impl PioPin>,
    debug_pin: Peri<'d, impl PioPin>,
) {
    // Receive: the line idles high, every bit starts with a falling edge,
    // and is low for 75 % of the bit for a 1 and for 37.5 % for a 0.
    // At 12 MHz a bit is 40 ticks, so sampling ~20 ticks after the edge reads
    // low for a 1 and high for a 0: the ISR gets the bits inverted.
    // See docs/rx-bit-timing.pdf.
    //
    // Reply: ~30 µs after the frame, drive the line and send the 21 levels from the TX FIFO,
    // 32 ticks (2.667 µs) each, then release the line.
    // See docs/rp2040-pin-switch-dshot.md.
    //
    // Debug pin (side-set): a 2-tick pulse around each bit sample, and high for the whole
    // time the PIO drives the line. Side-set takes 1 bit of the delay field, so delays
    // are at most [15]; longer ones are split with `nop` keeping the same tick counts.
    let prg = pio_asm!(
        ".side_set 1",
        ".wrap_target",
        "idle:",
        "  set y, 31           side 0",
        "idle_loop:",
        "  jmp pin still_high  side 0", // line high: keep counting
        "  jmp idle            side 0", // line low: start over
        "still_high:",
        "  jmp y-- idle_loop   side 0", // 32 × 2 ticks ≈ 5.3 µs of silence: between frames
        "  set x, 15           side 0",
        "bit:",
        "  wait 0 pin 0 [15]   side 0", // falling edge, then wait to the middle of the bit:
        "  nop [2]             side 0", // 16 + 3 ...
        "  nop                 side 1", // ... + 1 = 20 ticks; debug pulse starts
        "  in pins, 1          side 1", // 20 ticks after the edge
        "  wait 1 pin 0        side 0", // wait for the line to rise again
        "  jmp x-- bit         side 0", // 16 bits; X ends as 0xFFFFFFFF
        "  push noblock        side 0", // frame to the RX FIFO; if it is full the frame is dropped, the PIO never stalls
        "  set y, 22           side 0",
        "delay:",
        "  jmp y-- delay [15]  side 0", // 23 × 16 = 368 ticks ≈ 30.7 µs
        "  pull noblock        side 0", // reply from the TX FIFO, or X (all ones: line stays high) if empty
        "  set pins, 1         side 0", // level first ...
        "  set pindirs, 1      side 1", // ... then drive: no glitch before the start bit
        "  set y, 20           side 1",
        "tx:",
        "  out pins, 1 [15]    side 1", // 21 levels, 16 ...
        "  nop [14]            side 1", // ... + 15 ...
        "  jmp y-- tx          side 1", // ... + 1 = 32 ticks each
        "  set pins, 1         side 1", // pull the line up actively ...
        "  set pindirs, 0      side 0", // ... then release it to the pull-up
        ".wrap"
    );

    let mut dshot_pin = pio.make_pio_pin(dshot_pin);
    dshot_pin.set_pull(Pull::Up); // the line idles high
    let d_pin = pio.make_pio_pin(debug_pin);

    let mut cfg = Config::default();
    cfg.use_program(&pio.load_program(&prg.program), &[&d_pin]);

    // One pin in all four groups: the program both listens and talks on it.
    cfg.set_in_pins(&[&dshot_pin]); // `wait`, `in`
    cfg.set_jmp_pin(&dshot_pin); // `jmp pin`
    cfg.set_out_pins(&[&dshot_pin]); // `out pins`: reply levels
    cfg.set_set_pins(&[&dshot_pin]); // `set pins`, `set pindirs`: direction switch
    cfg.shift_in = ShiftConfig {
        auto_fill: false, // no autopush: it would stall the PIO on a full RX FIFO
        direction: ShiftDirection::Left,
        threshold: 16,
    };
    cfg.shift_out = ShiftConfig {
        auto_fill: false,                // only `pull noblock` takes data from the TX FIFO
        direction: ShiftDirection::Left, // `out pins, 1` takes bit 31 first
        threshold: 32,
    };
    cfg.clock_divider =
        fixed::FixedU32::<U8>::from_num(clk_sys_freq() as f32 / PIO_CLOCK_HZ as f32);

    sm.set_config(&cfg);
    sm.set_pin_dirs(Direction::In, &[&dshot_pin]); // start as input
    sm.set_pin_dirs(Direction::Out, &[&d_pin]);
    sm.set_enable(true);
}

async fn pio_task_sm0(mut sm: StateMachine<'static, PIO0, 0>) -> ! {
    // The reply logic lives in esc-emu (testable on the PC); this task only moves
    // values between the PIO FIFOs and the emulated ESC, and counts.
    let mut esc = Esc::new(EDT_AT_START);
    sm.tx().push(esc.first_reply()); // before the first frame: "motor stopped"
    let mut last_wake: Option<Instant> = None;
    // Totals since start, also sent as EDT debug 1 / debug 2.
    let mut counters = Counters::default();
    loop {
        let mut raw = sm.rx().wait_pull().await;
        let now = Instant::now();
        let gap_us = last_wake.map(|t| (now - t).as_micros());
        last_wake = Some(now);

        // If this task was late, several frames are waiting: count them all,
        // but queue a single reply, for the newest one (see docs/rx-overflow-plan.md).
        let mut frames = 0u32;
        let mut bad_crc = 0u32;
        let mut result;
        loop {
            let rx = esc.on_raw(raw);
            result = rx.result;
            frames += 1;
            if result.is_err() {
                bad_crc += 1;
            }
            if let Some(e12) = rx.edt_event {
                defmt::info!("EDT {=bool}: reply {=u16:#x}", esc.edt_enabled(), e12);
            }
            match sm.rx().try_pull() {
                Some(next) => raw = next,
                None => break,
            }
        }
        counters.bad = counters.bad.wrapping_add(bad_crc);
        if gap_us.is_some_and(|gap_us| gap_us > LATE_GAP_US) {
            counters.late = counters.late.wrapping_add(1);
        }
        let queued = sm.tx().try_push(esc.next_reply(counters)); // for the next frame

        STATS.lock(|cell| {
            let mut s = cell.get();
            s.last_raw = Some(raw);
            s.last = Some(result);
            s.frames = s.frames.wrapping_add(frames);
            s.bad_crc = s.bad_crc.wrapping_add(bad_crc);
            if !queued {
                s.tx_full = s.tx_full.wrapping_add(1);
            }
            if let Some(gap_us) = gap_us {
                if gap_us > LATE_GAP_US {
                    s.late = s.late.wrapping_add(1);
                }
                s.max_gap_us = s.max_gap_us.max(gap_us.min(u32::MAX as u64) as u32);
            }
            s.edt = esc.edt_enabled();
            cell.set(s);
        });
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    defmt::info!("dshot-reader-embassy: DShot300 on GPIO0, debug on GPIO3");

    // Instantiate the USB driver
    let driver = Driver::new(p.USB, Irqs);

    // Create embassy-usb Config
    let mut config = UsbConfig::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Taras");
    config.product = Some("DShot logger");
    config.serial_number = Some("12345678");
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut [], // no msos descriptors
        &mut control_buf,
    );

    // Communication Device Class (CDC) USB Device using a subclass Abstract Control Model (ACM)
    let class = CdcAcmClass::new(&mut builder, &mut state, 64);

    // Build the USB Device.
    let mut usb = builder.build();

    // We need only the writer to send bytes over it.
    let (mut usb_tx, _usb_rx) = class.split();

    // Lines go out only while a terminal holds the port open (DTR set): nothing is queued
    // while it is closed, so the first line after opening is a fresh one (docs/day-007.md).
    let usb_future = async {
        loop {
            wait_for_terminal(&mut usb_tx).await;
            let _ = usb_write(&mut usb_tx).await;
        }
    };

    // Setup PIO SM0
    let Pio {
        mut common,
        mut sm0,
        ..
    } = Pio::new(p.PIO0, Irqs);

    setup_pio_task_sm0(&mut common, &mut sm0, p.PIN_0, p.PIN_3);

    // Join all 3 futures
    join3(usb.run(), usb_future, pio_task_sm0(sm0)).await;
}

struct Disconnected {}

impl From<EndpointError> for Disconnected {
    fn from(val: EndpointError) -> Self {
        match val {
            EndpointError::BufferOverflow => panic!("Buffer overflow"),
            EndpointError::Disabled => Disconnected {},
        }
    }
}

/// One text line for USB.
struct Line {
    buf: [u8; 128],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Line {
            buf: [0; 128],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Formats a snapshot of [`Stats`], e.g.
/// `7D05 thr=1000 T=0 cmd=0 n=12345 bad=0 txf=0 late=0 gap=251 edt=0` or
/// `7D04 BAD_CRC n=12346 bad=1 txf=0 late=0 gap=251 edt=0`.
fn format_stats(s: &Stats) -> Line {
    let mut line = Line::new();
    // The longest line is 105 bytes, so these writes cannot run out of space.
    let _ = match (s.last_raw, s.last) {
        (Some(raw), Some(Ok(f))) => write!(
            line,
            "{:04X} thr={} T={} cmd={} ",
            !raw as u16,
            f.throttle,
            f.telemetry as u8,
            f.is_command() as u8
        ),
        (Some(raw), Some(Err(FrameError::BadCrc))) => write!(line, "{:04X} BAD_CRC ", !raw as u16),
        _ => write!(line, "no frames "),
    };
    let _ = write!(
        line,
        "n={} bad={} txf={} late={} gap={} edt={}\r\n",
        s.frames, s.bad_crc, s.tx_full, s.late, s.max_gap_us, s.edt as u8
    );
    line
}

/// Largest chunk sent in one USB packet. Kept below the 64-byte packet size:
/// a full-size packet would need a zero-length packet after it
/// for the host to deliver the line right away.
const USB_CHUNK: usize = 63;

/// Waits until a terminal opens the port. `wait_connection` alone is not enough: it waits
/// only for the host to configure the USB device and then returns at once every time, so a
/// loop around it would never give the PIO task a turn while the port is closed.
async fn wait_for_terminal<'d, T: Instance + 'd>(usb_tx: &mut Sender<'d, Driver<'d, T>>) {
    usb_tx.wait_connection().await;
    while !usb_tx.dtr() {
        Timer::after_millis(DTR_POLL_MS).await;
    }
}

/// Write the latest frame and counters to USB as text, every [`REPORT_PERIOD_MS`], while
/// the port is open. Returns when the port is closed or the host stops taking packets.
async fn usb_write<'d, T: Instance + 'd>(
    usb_tx: &mut Sender<'d, Driver<'d, T>>,
) -> Result<(), Disconnected> {
    // `gap` from the moment the port was opened: the time it stayed closed is not shown.
    STATS.lock(|cell| {
        cell.set(Stats {
            max_gap_us: 0,
            ..cell.get()
        })
    });
    loop {
        Timer::after_millis(REPORT_PERIOD_MS).await;
        if !usb_tx.dtr() {
            return Err(Disconnected {});
        }
        let stats = STATS.lock(|cell| {
            let s = cell.get();
            cell.set(Stats { max_gap_us: 0, ..s }); // `gap` covers one report period
            s
        });
        for chunk in format_stats(&stats).as_bytes().chunks(USB_CHUNK) {
            let write = usb_tx.write_packet(chunk);
            match with_timeout(Duration::from_millis(USB_WRITE_TIMEOUT_MS), write).await {
                Ok(written) => written?,
                // The host does not take packets: drop the line.
                Err(_) => return Err(Disconnected {}),
            }
        }
    }
}
