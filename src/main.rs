#![no_std]
#![no_main]

#[unsafe(link_section = ".boot_loader")]
#[used]
pub static BOOT2_FIRMWARE: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

use panic_halt as _;

use core::cell::Cell;
use core::fmt::Write as _;
use dshot_codec::{decode_frame, Frame, FrameError};
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
use embassy_rp::{bind_interrupts, pio::InterruptHandler as PioInterruptHandler};
use embassy_sync::blocking_mutex::{raw::ThreadModeRawMutex, Mutex};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{CdcAcmClass, Sender, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config as UsbConfig};
use fixed::types::extra::U8;

/// PIO state machine clock: 12 MHz gives whole numbers of ticks for both
/// the command bit (3.333 µs = 40 ticks) and the reply bit (2.667 µs = 32 ticks).
const PIO_CLOCK_HZ: u32 = 12_000_000;

/// How often the latest frame and counters are sent over USB.
const REPORT_PERIOD_MS: u64 = 500;

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
}

static STATS: Mutex<ThreadModeRawMutex, Cell<Stats>> = Mutex::new(Cell::new(Stats {
    last_raw: None,
    last: None,
    frames: 0,
    bad_crc: 0,
}));

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
});

// Setup PIO SM0: receive inverted (bidirectional) DShot300 frames.
fn setup_pio_task_sm0<'d>(
    pio: &mut Common<'d, PIO0>,
    sm: &mut StateMachine<'d, PIO0, 0>,
    dshot_pin: impl PioPin,
    debug_pin: impl PioPin,
) {
    // Bidirectional DShot: the line idles high, every bit starts with a falling edge,
    // and is low for 75 % of the bit for a 1 and for 37.5 % for a 0.
    // At 12 MHz a bit is 40 ticks, so sampling ~20 ticks after the edge reads
    // low for a 1 and high for a 0: the ISR gets the bits inverted.
    // The debug pin pulses high around each sample point.
    let prg = pio_asm!(
        ".wrap_target",
        "idle:",
        "  set y, 31",
        "idle_loop:",
        "  jmp pin still_high", // line high: keep counting
        "  jmp idle",           // line low: start over
        "still_high:",
        "  jmp y-- idle_loop", // 32 × 2 ticks ≈ 5.3 µs of silence: between frames
        "  set x, 15",
        "bit:",
        "  wait 0 pin 0 [18]", // falling edge, then wait to the middle of the bit
        "  set pins, 1",       // debug: sample point
        "  in pins, 1",        // 20 ticks after the edge
        "  set pins, 0",
        "  wait 1 pin 0", // wait for the line to rise again
        "  jmp x-- bit",  // 16 bits; autopush hands the frame to the RX FIFO
        ".wrap"
    );

    let mut cfg = Config::default();
    cfg.use_program(&pio.load_program(&prg.program), &[]);

    let mut dshot_pin = pio.make_pio_pin(dshot_pin);
    dshot_pin.set_pull(Pull::Up); // the line idles high
    let d_pin = pio.make_pio_pin(debug_pin);

    cfg.set_in_pins(&[&dshot_pin]); // for `wait` and `in`
    cfg.set_jmp_pin(&dshot_pin); // for `jmp pin`
    cfg.set_set_pins(&[&d_pin]);
    cfg.shift_in = ShiftConfig {
        auto_fill: true,
        direction: ShiftDirection::Left,
        threshold: 16,
    };
    cfg.clock_divider =
        fixed::FixedU32::<U8>::from_num(clk_sys_freq() as f32 / PIO_CLOCK_HZ as f32);

    sm.set_config(&cfg);
    sm.set_pin_dirs(Direction::In, &[&dshot_pin]);
    sm.set_pin_dirs(Direction::Out, &[&d_pin]);
    sm.set_enable(true);
}

async fn pio_task_sm0(mut sm: StateMachine<'static, PIO0, 0>) -> ! {
    loop {
        let raw = sm.rx().wait_pull().await;
        let result = decode_frame(raw);
        STATS.lock(|cell| {
            let mut s = cell.get();
            s.last_raw = Some(raw);
            s.last = Some(result);
            s.frames = s.frames.wrapping_add(1);
            if result.is_err() {
                s.bad_crc = s.bad_crc.wrapping_add(1);
            }
            cell.set(s);
        });
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

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

    // Wait for a connection and write to USB
    let usb_future = async {
        loop {
            usb_tx.wait_connection().await;
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

/// One text line, short enough for a single USB packet.
///
/// Kept below 64 bytes: a full-size packet would need a zero-length packet after it
/// for the host to deliver the line right away.
struct Line {
    buf: [u8; 63],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Line {
            buf: [0; 63],
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
/// `7D05 thr=1000 T=0 cmd=0 n=12345 bad=0` or `7D04 BAD_CRC n=12346 bad=1`.
fn format_stats(s: &Stats) -> Line {
    let mut line = Line::new();
    // The longest line is 53 bytes, so these writes cannot run out of space.
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
    let _ = write!(line, "n={} bad={}\r\n", s.frames, s.bad_crc);
    line
}

/// Write the latest frame and counters to USB as text.
async fn usb_write<'d, T: Instance + 'd>(
    usb_tx: &mut Sender<'d, Driver<'d, T>>,
) -> Result<(), Disconnected> {
    loop {
        Timer::after_millis(REPORT_PERIOD_MS).await;
        let stats = STATS.lock(|cell| cell.get());
        usb_tx.write_packet(format_stats(&stats).as_bytes()).await?;
    }
}
