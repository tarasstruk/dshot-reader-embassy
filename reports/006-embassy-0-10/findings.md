# 006 — embassy 0.9 → 0.10: результати

**Дата:** 2026-10-19. **Гілка:** `with-probe-rs`. **Коміт міграції:** `7b4e5d4`. **Збірка:** release, прошита через probe-rs. **FC:** TBS Lucid, Betaflight 2026.6.2, `dshot_edt = FORCE`; RP2040 на виході мотора 4.

План — `docs/day-006.md`. Попередній перехід (0.4 → 0.9) — `reports/004-with-probe-rs/migration-stage-2.md`.

> **Коротко.**
> - Міграція — лише три рядки в `Cargo.toml`; код не змінювався.
> - Дільник PIO той самий, що в 0.9: «нова математика дільника» в embassy-rp 0.10 нас не стосується.
> - Стенд: усі значення телеметрії збігаються з днем 005 до одиниці; Invalid 0 %; RP2040 — `bad = 0`, `txf = 0`, `late = 0`, `gap` 255–295 мкс; USB (embassy-usb 0.6) працює.

## 1. Що змінилося в коді

```diff
-embassy-executor = { version = "0.9.0", features = ["arch-cortex-m", "executor-thread"] }
+embassy-executor = { version = "0.10.0", features = ["platform-cortex-m", "executor-thread"] }
-embassy-rp = { version = "0.9.0", …
+embassy-rp = { version = "0.10.0", …
-embassy-usb = "0.5.1"
+embassy-usb = "0.6.0"
```

У embassy-executor 0.10 features `arch-*` перейменовано на `platform-*`. Більше нічого правити не довелося: `src/main.rs`, `dshot-codec` і `esc-emu` без змін.

| Пакет | Було | Стало |
|---|---|---|
| `embassy-rp` | 0.9.0 | **0.10.0** |
| `embassy-executor` | 0.9.1 | **0.10.0** |
| `embassy-usb` | 0.5.1 | **0.6.0** |
| `embassy-usb-driver` | 0.2.2 | 0.2.2 |
| `embassy-time` | 0.5.1 | 0.5.1 |
| `embassy-sync` | 0.8.0 | 0.8.0 |
| `embassy-futures` | 0.1.2 | 0.1.2 |

Дублікати в дереві залежностей (`embassy-hal-internal` 0.4 і 0.5, `heapless`, `embedded-io` тощо) приходять зсередини `embassy-rp` 0.10 (через `embassy-embedded-hal` 0.6) і на нас не впливають.

**Перевірки:** збірка debug і release (у бінарнику `embassy-rp-0.10.0`), clippy без попереджень, `cargo fmt` чистий, `cargo test-codec` (23), `cargo test-esc-emu` (10).

## 2. Дільник PIO

Головний ризик з changelog — «Improve PIO clock divider math» в embassy-rp 0.10. Перевірено в джерелах обох версій:

| Що | 0.9 | 0.10 |
|---|---|---|
| запис у `CLKDIV` у `set_config` | `clock_divider.to_bits() << 8` | так само |
| `clk_sys` за замовчуванням (`ClockConfig::crystal`) | 125 МГц | той самий код, 125 МГц |
| «нова математика» | — | лише допоміжна `pio_programs::clock_divider::calculate_pio_clock_divider` для готових PIO-програм embassy |

Ми цю функцію не використовуємо: дільник рахує `src/main.rs` (`FixedU32::<U8>::from_num(clk_sys_freq() as f32 / 12e6)`). Значення, пораховане на ПК з `fixed` 1.31.0 (як у прошивці):

| Спосіб | Біти | Дільник | Такт PIO |
|---|---|---|---|
| наш | `0xA6B` | 10 + 107/256 | **11 998 500 Гц** (−0,0125 %) |
| функція embassy | `0xA6A` | 10 + 106/256 | 12 003 001 Гц (+0,025 %) |

Наш дільник той самий, що в 0.9; переходити на функцію embassy немає сенсу. Порядок `set_config` → `set_pin_dirs` (новий embassy-rp панікує при іншому) у `setup_pio_task_sm0` уже правильний.

## 3. Стенд

**Прошивання** (`flash.log`): probe-rs, 1,81 с; лог старту `[INFO ] dshot-reader-embassy: DShot300 on GPIO0, debug on GPIO3 (…/src/main.rs:235)`.

**Прогін через CLI** (`sweep_cli.log`, скрипт `reports/004-with-probe-rs/hil/sweep_cli.py`). FC — `/dev/ttyACM0`, RP2040 — `/dev/ttyACM1`.

| `motor 3 …` | Газ | Type | eRPM | RPM | TEMP | VCC | CURR | ST/EV | Invalid | День 005 |
|---|---|---|---|---|---|---|---|---|---|---|
| 1001 | 48 | `RTVCS` | 2 000 | 286 | 27 | 16.50 | 0 | 0 | 0,00 % | ✅ |
| 1250 | 546 | `RTVCS` | 11 900 | 1 700 | 42 | 16.00 | 4 | 4 | 0,00 % | ✅ |
| 1500 | 1046 | `RTVCS` | 21 900 | 3 129 | 56 | 15.25 | 9 | 7 | 0,00 % | ✅ |
| 1750 | 1546 | `RTVCS` | 32 000 | 4 571 | 70 | 14.50 | 14 | 75 | 0,00 % | ✅ |
| 2000 | 2047 | `RTVCS` | 42 000 | 6 000 | 84 | 14.00 | 20 | 206 | 0,00 % | ✅ |
| 1000 | 0 | `RTVCS` | 0 | 0 | 25 | 16.75 | 0 | 0 | 0,00 % | ✅ |

«День 005» — порівняння з `reports/005-more-telemetry/findings.md`, розділ 1: усі значення ідентичні.

**RP2040** на кожному кроці:

| Що | Значення |
|---|---|
| `thr` | точно той газ, що надіслав FC |
| `bad`, `txf`, `late` | 0, 0, 0 |
| `gap` | 255–295 мкс — задача PIO прокидається на кожен кадр |
| `edt` | 1 |
| USB | рядки читаються, embassy-usb 0.6 працює |

Наприкінці — `motor 3 1000` і `exit` (FC перезавантажився).

Оскільки прийом (газ), відповідь (оберти й EDT декодуються з Invalid 0 %) і тривалість циклу збігаються з днем 005, таймінги PIO (вибірка, біт відповіді) працюють так само, як в 0.9 — що й очікувано з однаковим дільником.

## 4. Що не робили

- Крок 4 плану — осцилограф (голка вибірки, біт відповіді) — за домовленістю пропущено.

## 5. Висновок

Міграцію на embassy 0.10 завершено: збірка, тести і стенд — без змін у поведінці. Embassy 0.10 — тепер робоча версія.

## 6. Файли

| Файл | Що |
|---|---|
| `flash.log` | вивід `probe-rs run` при прошиванні |
| `sweep_cli.log` | вивід `sweep_cli.py` |
