# AGENTS.md

Guidance for AI coding agents working in this workspace.

## Project Overview

Bare-metal Rust firmware that turns an **RP2350** (Raspberry Pi Pico 2, RP2350A, Cortex-M33 cores) into an emulated **ESC** for **bidirectional DShot300**. A real flight controller (Betaflight) drives the RP2350 like a motor ESC: the RP2350 receives each DShot frame, decodes the throttle, and ~30 µs later answers on the same wire with telemetry — eRPM and Extended DShot Telemetry (EDT: temperature, voltage, current, status, debug). The values are bench toy models, there is no motor. Embassy async runtime (`#![no_std]`, `#![no_main]`), embassy-rp **0.10**.

Branch: work happens on **`with-rp-2350`**; the RP2040 (RP2040-Zero) version stays on `with-probe-rs`. Do not propose merging branches.

## Workspace

```
dshot-reader-embassy (root package, firmware binary, RP2350)
 ├── depends on esc-emu    (path)
 └── depends on dshot-codec (path)
esc-emu      → dshot-codec
dshot-codec  → (no dependencies)
```

| Package | Kind | What it is |
|---|---|---|
| `dshot-reader-embassy` (root, `src/main.rs`) | binary, `no_std`/`no_main`, RP2350 only | hardware: PIO program, PIO FIFOs, USB CDC text output, defmt logs, counters |
| `esc-emu` (`esc-emu/src/lib.rs`) | library, `no_std` (std in tests) | the emulated ESC: what to answer to each frame — eRPM model, EDT models and schedule (`Esc`) |
| `dshot-codec` (`dshot-codec/src/lib.rs`) | library, `no_std` (std in tests) | protocol only: frame CRC/decode/encode, e12 period, GCR/NRZI reply, EDT value encoding/decoding, `EdtSwitch` |

Keep this split: protocol in `dshot-codec`, bench/ESC policy in `esc-emu`, anything touching hardware in `src/main.rs`. Both libraries must stay hardware-independent so they can be tested on the PC. Do **not** add an `integration` package.

## Architecture

### Firmware (`src/main.rs`)

- **PIO0 SM0** (`setup_pio_task_sm0`): one program that receives a frame on **GPIO0**, waits ≈ 31 µs, drives the line and sends the 21-level reply from the TX FIFO, then releases the line. PIO clock 12 MHz (`PIO_CLOCK_HZ`; clk_sys 150 MHz, divider 12.5 = `0xC80` → exactly 12 000 000 Hz): command bit = 40 ticks, reply bit = 32 ticks. Sampling ≈ 1.7 µs after each falling edge. No autopush: `push noblock`, so a slow CPU drops frames instead of stalling the PIO.
- **GPIO3** — debug output via side-set: a ~170 ns pulse at each bit sample, high for the whole reply.
- **`pio_task_sm0`**: on each wake-up drains the RX FIFO into `Esc::on_raw`, pushes one `Esc::next_reply(counters)` into the TX FIFO (reply is always one frame ahead), updates `STATS`.
- **USB CDC** (VID `c0de`, product `DShot logger`): one text line every 500 ms, e.g. `7D05 thr=1000 T=0 cmd=0 n=12345 bad=0 txf=0 late=0 gap=251 edt=1` — **only while a terminal holds the port open (DTR)**. Nothing is queued while it is closed, so the first line after opening is fresh (≈ 0.5 s after opening); `gap` is reset when the port is opened. `wait_connection()` alone does not wait for DTR (it returns at once once USB is configured) — `wait_for_terminal` polls DTR with a `Timer`. Each packet is written with a 50 ms timeout (day 007).
- **defmt over RTT**: start-up line and EDT on/off events, visible through the debug probe.
- `EDT_AT_START = true`: EDT on from boot (Betaflight with `dshot_edt = FORCE` never sends DShot command 13 on a bench FC that cannot arm).

### Telemetry models (`esc-emu`)

- eRPM = 1000 + throttle × 20 (commands 0–47 → "motor stopped", e12 `0xFFF`).
- Every 32nd reply (`EDT_EVERY`) is one EDT value, in turn: temperature (25 → 85 °C over 0–42 000 eRPM), voltage (16.8 → 14.0 V), current (0–20 A by throttle), status (warning ≥ 70 °C, alert ≥ 80 °C), debug 1 (`bad` low byte), debug 2 (`late` low byte).

Details: `docs/edt.md`, `docs/day-005.md`. Move to the RP2350: `docs/day-008.md`.

## Build, Test, Flash

```sh
cargo build --release         # firmware (default target thumbv8m.main-none-eabihf)
cargo clippy                  # firmware lint
cargo test-codec              # host tests of dshot-codec  (alias in .cargo/config.toml)
cargo test-esc-emu            # host tests of esc-emu
cargo run --release           # flash via probe-rs (Raspberry Pi Debug Probe), then defmt logs; Ctrl-C to stop
probe-rs list                 # is the probe there?
```

- **Flashing is only through the probe** (`probe-rs`); no BOOTSEL/UF2 path.
- With the `test-esc` FC on the bench, two probes are plugged in: flash with `cargo build --release && probe-rs run --chip RP235x --probe 2e8a:000c:E665D0C59B25A52B target/thumbv8m.main-none-eabihf/release/dshot-reader-embassy` (the runner names no probe; see the device table below).
- **Always flash the release build.** The debug build delays the PIO task by ≈ 1.7 ms on every USB line (`reports/003-day-three/findings.md`).
- Library clippy for tests: `cargo clippy -p esc-emu --target x86_64-unknown-linux-gnu --tests` (same for `dshot-codec`).
- Linker args (`-Tlink.x -Tdefmt.x`) live in `.cargo/config.toml`. The RP2350 boot ROM needs the IMAGE_DEF block in the first 4 KiB of flash; embassy-rp provides it (`.start_block`, placed by `memory.x`).

Before handing over a code change: release build, clippy, `cargo fmt --check`, both host test aliases.

## Hardware Bench

| Device | How to find it |
|---|---|
| FC (Betaflight, AT32F435) | `/dev/ttyACM*` with `ID_VENDOR_ID=2e3c` |
| RP2350 (this firmware) | `/dev/ttyACM*` with `ID_VENDOR_ID=c0de` |
| Debug probe of this board (ESC) | `probe-rs … --probe 2e8a:000c:E665D0C59B25A52B`; its UART: `/dev/ttyACM*` with `ID_VENDOR_ID=2e8a`, `ID_SERIAL_SHORT=E665D0C59B25A52B` |
| Debug probe of the `test-esc` FC | `probe-rs … --probe 2e8a:000c:E66480454F2FB036`; its UART: `ID_SERIAL_SHORT=E66480454F2FB036` — do not flash this firmware through it |

Find ports by VID (`udevadm info -q property -n /dev/ttyACMx | grep ID_VENDOR_ID`); the numbers change after every FC reboot.

- RP2350 is on the FC's **motor 4** output (CLI index **3**). FC settings: `DSHOT300`, `dshot_bidir = ON`, `dshot_edt = FORCE`, `motor_poles = 14`.
- **Talk to the FC through its CLI** (`#`, `motor 3 <1000..2000>`, `dshot_telemetry_info`, `get`/`set`, then `exit`). `exit` reboots the FC — that is fine. Use MSP (`reports/004-with-probe-rs/hil/fc_status.py`, `sweep_msp.py`) **only with the user's explicit consent**.
- Standard bench test: `sg dialout -c "python3 reports/004-with-probe-rs/hil/sweep_cli.py <FC> <RP2350> 1001,1250,1500,1750,2000"` — sets motor 4 throttle, reads `dshot_telemetry_info` and the RP2350 line, returns throttle to 1000, then `exit`.
- Serial ports belong to group `dialout`; if the session does not have it yet, wrap commands in `sg dialout -c "…"`.
- **Closing** the RP2350 port can hang for tens of seconds: open it once and kill the reader at the end (as `sweep_cli.py` does). If a port hangs, `probe-rs reset --chip RP235x` or replug.
- Only one program can hold the probe ("interface is busy"): check `pgrep -a probe-rs`. Run `probe-rs run` with `timeout -s INT …` and output to a file, not piped through `tail`.
- No propellers: a real ESC on that output would spin.
- When done, release all ports and the probe.

## Docs and Reports

- `docs/` — plans and explanations (mostly in Ukrainian): `action-plan.md` (overall stages), `edt.md`, `rx-overflow-plan.md`, `day-NNN.md` (plan of a work day), protocol notes and PDFs.
- `reports/NNN-<topic>/findings.md` — what was observed on the bench, with logs, scope SVG/JPG files and tables comparing measured vs. expected values.
- New work day: write the plan into `docs/day-NNN.md` first (steps, expected values, commit per step), then do the steps.
- `docs/action-plan-no-hardware.md` is history only; do not propose emulators (rp2040pio, rp2040js) or FC models.

## Conventions

- **Language:** talk to the user in Ukrainian; code, code comments and commit messages in English; docs and reports in Ukrainian.
- **Comments** match the surrounding density; explain hardware timing with tick counts.
- **No heap**, no `std` in the firmware and libraries (except tests). Logging via `defmt`.
- Integer math in models, fractions dropped; constants named and documented.
- Tests in the libraries pin exact values (e12 codes, table rows from the docs).

## Commits

- **Never commit on your own.** Before every commit show the files, a short summary and the exact message, then wait for an explicit yes for that commit.
- Messages: short, one line, English, **no `Co-Authored-By` trailer**.
- One commit per plan step unless the user says otherwise.

## Gotchas

- IDE (rust-analyzer) reports `can't find crate for test` / `cannot find macro assert_eq` in the libraries: it checks tests for `thumbv8m.main-none-eabihf`. Not a real error — the host test aliases pass.
- Betaflight `motor 3 <v>` prints the resulting DShot throttle (48 + (v − 1001) × 1999 / 999, fraction dropped), not the input.
- Betaflight shows eRPM rounded to hundreds and RPM = eRPM / 7; ST/EV is the status byte in decimal.
- The FC sends DShot command 13 only when arming; the bench FC cannot arm (no receiver, no telemetry from motors 1–3).
