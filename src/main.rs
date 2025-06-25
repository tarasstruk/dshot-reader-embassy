#![no_std]
#![no_main]

#[unsafe(link_section = ".boot_loader")]
#[used]
pub static BOOT2_FIRMWARE: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

use panic_halt as _;

use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_rp::peripherals::{PIO0, USB};
use embassy_rp::pio::program::pio_asm;
use embassy_rp::pio::{
    Common, Config, Direction, Pio, PioPin, ShiftConfig, ShiftDirection, StateMachine,
};
use embassy_rp::usb::{Driver, Instance, InterruptHandler as UsbInterruptHandler};
use embassy_rp::{bind_interrupts, pio::InterruptHandler as PioInterruptHandler};
use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{CdcAcmClass, Sender, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config as UsbConfig};
use fixed::types::extra::U8;
static SIGNAL: Signal<ThreadModeRawMutex, u32> = Signal::new();

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
});

// Setup PIO SM0
fn setup_pio_task_sm0<'d>(
    pio: &mut Common<'d, PIO0>,
    sm: &mut StateMachine<'d, PIO0, 0>,
    input_pin: impl PioPin,
    debug_pin: impl PioPin,
) {
    let prg = pio_asm!(
        ".wrap_target",
        "  set x, 1"
        "  set pins, 0"
        "reset:",
        "  set y, 31"
        "  wait 0 pin 0",
        "loop:",
        "  jmp pin reset",
        "  jmp y-- loop"
        "begin:",
        "  set pins, 0",
        "  wait 1 pin 0",
        "  nop [12]"
        "  jmp pin one",
        "zero:",
        "  in null, 1",
        "  jmp begin",
        "one:",
        "  set pins, 1"
        "  in x, 1",
        "  wait 0 pin 0",
        "  jmp begin",
        ".wrap"
    );

    let mut cfg = Config::default();
    cfg.use_program(&pio.load_program(&prg.program), &[]);
    let in_pin = pio.make_pio_pin(input_pin);
    let d_pin = pio.make_pio_pin(debug_pin);

    cfg.set_jmp_pin(&in_pin);
    cfg.shift_in = ShiftConfig {
        auto_fill: true,
        direction: ShiftDirection::Left,
        threshold: 16,
    };

    cfg.clock_divider = fixed::FixedU32::<U8>::from_num(10);
    cfg.set_set_pins(&[&d_pin]);
    sm.set_config(&cfg);
    sm.set_pin_dirs(Direction::Out, &[&d_pin]);
    sm.set_enable(true);
}

async fn pio_task_sm0(mut sm: StateMachine<'static, PIO0, 0>) -> ! {
    loop {
        let value = sm.rx().wait_pull().await;
        SIGNAL.signal(value);
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

/// Write signals to USB
async fn usb_write<'d, T: Instance + 'd>(
    usb_tx: &mut Sender<'d, Driver<'d, T>>,
) -> Result<(), Disconnected> {
    loop {
        Timer::after_millis(1000).await;
        let v = SIGNAL.wait().await;
        usb_tx.write_packet(&v.to_be_bytes()).await?;
    }
}
