# 004, етап 2 — оновлення залежностей і embassy 0.9

**Дата:** 2026-10-17. **Гілка:** `with-probe-rs`.

Продовження `migration.md` (етап 1: прошивання через probe-rs). Мета етапу 2 — оновити залежності, а embassy перевести на той самий набір версій, що в `/home/taras/learn/gabriele-embassy` (embassy-rp 0.9), щоб мати перевірене поєднання. Як і раніше, WiFi (`cyw43`, `embassy-net`) не переноситься.

> **Коротко.**
> - **Крок 1, `cargo update`:** лише сумісні оновлення в `Cargo.lock`, без змін у коді (окремий коміт).
> - **Крок 2, embassy 0.4 → 0.9:** у коді змінилися лише boot2 і тип пінів `Peri`. API PIO, USB і executor скомпілювалися без змін.
> - Прошивка збирається, проходить тести, прошивається через probe-rs і стартує; USB піднімається.
> - **На стенді з FC ще не перевірено.**

## 1. Які пакети були застарілі

Перевірка: `cargo search <пакет>` (остання версія на crates.io) проти `Cargo.lock`.

| Пакет | Було в `Cargo.lock` | Остання | Стан |
|---|---|---|---|
| `embassy-rp` | 0.4.0 | 0.10.0 | сильно застарів |
| `embassy-executor` | 0.7.0 | 0.10.0 | сильно застарів |
| `embassy-usb` | 0.4.0 | 0.6.0 | сильно застарів |
| `embassy-time` | 0.4.0 | 0.5.1 | застарів |
| `embassy-sync` | 0.7.0 (і 0.6.2 транзитивно) | 0.8.0 | застарів |
| `embassy-futures` | 0.1.1 | 0.1.2 | патч |
| `cortex-m-rt` | 0.7.5 | 0.7.7 | патч |
| `defmt`, `defmt-rtt`, `panic-probe`, `rp2040-boot2` | — | — | актуальні |
| `fixed` | 1.29.0 | 1.29.0 (є 2.0.0-alpha) | остання стабільна |

У 0.x кожна мінорна версія (0.4 → 0.5) може ламати API, тому embassy оновлюється окремим кроком і всі його пакети разом.

## 2. Крок 1: `cargo update`

```
cargo update
```

`cargo update` змінює лише `Cargo.lock`: піднімає кожну залежність до найновішої версії, сумісної з вимогою в `Cargo.toml` (`"0.7.5"` у Cargo означає `^0.7.5`, тобто будь-яка 0.7.x ≥ 0.7.5).

| Що оновилося | Версії |
|---|---|
| `cortex-m-rt` | 0.7.5 → 0.7.7 |
| `embassy-futures` | 0.1.1 → 0.1.2 |
| `embassy-sync` (наш 0.7.x) | 0.7.0 → 0.7.2 |
| транзитивні | `serde`, `syn`, `zerocopy`, `typenum`, `smallvec` та інші, разом ≈ 75 пакетів; прибрано старі `windows-targets` 0.52 |

Код і `Cargo.toml` не змінювалися. Перевірка: `cargo build --release`, `cargo clippy`, `cargo test-codec` (20 тестів) — без помилок.

## 3. Крок 2: embassy 0.9

### 3.1. Версії (`Cargo.toml`)

Набір узято з `gabriele-embassy/Cargo.toml` і `Cargo.lock`. `embassy-usb` там немає, тож версію вибрано за сумісністю: `embassy-rp` 0.9 використовує `embassy-usb-driver` 0.2, і саме з ним працює `embassy-usb` 0.5.

```diff
-cortex-m-rt = "0.7.5"
+cortex-m-rt = "0.7.7"
-embassy-executor = { version = "0.7.0", features = ["arch-cortex-m", "executor-thread"] }
-embassy-futures = "0.1.1"
-embassy-rp = { version = "0.4.0", features = ["time-driver", "critical-section-impl", "rp2040"] }
-embassy-sync = "0.7.0"
-embassy-time = "0.4.0"
-embassy-usb = "0.4.0"
+embassy-executor = { version = "0.9.0", features = ["arch-cortex-m", "executor-thread"] }
+embassy-futures = "0.1.2"
+embassy-rp = { version = "0.9.0", features = ["time-driver", "critical-section-impl", "rp2040"] }
+embassy-sync = "0.8.0"
+embassy-time = "0.5.0"
+embassy-usb = "0.5.1"
 …
-rp2040-boot2 = "0.3.0"
```

`cortex-m-rt` і `embassy-futures` у `Cargo.toml` підняті до версій, які вже стояли в `Cargo.lock` після кроку 1, щоб вимога не була нижчою за фактичну версію.

Розв'язані версії в `Cargo.lock`:

| Пакет | Версія |
|---|---|
| `embassy-rp` | 0.9.0 |
| `embassy-executor` | 0.9.1 |
| `embassy-time` | 0.5.1 |
| `embassy-usb` | 0.5.1 |
| `embassy-usb-driver` | 0.2.2 |
| `embassy-sync` | 0.8.0 (наш), 0.7.2 (транзитивно, через `embassy-usb`) |
| `embassy-futures` | 0.1.2 |

Features ті самі, що й раніше: `time-driver`, `critical-section-impl`, `rp2040`. Feature `defmt` для embassy не вмикається: він лише додає реалізації `defmt::Format` для типів embassy, а ми їх не логуємо.

### 3.2. Перша збірка: які помилки

Після зміни версій `cargo build` дав п'ять помилок, двох видів:

| Помилка | Причина |
|---|---|
| `cannot find module or crate rp2040_boot2` | залежність `rp2040-boot2` прибрано (3.3) |
| `the trait PioPin is not implemented for Peri<'_, PIN_0>` (×2) | `p.PIN_0` у 0.9 має тип `Peri<'_, PIN_0>` (3.4) |
| `make_pio_pin: expected Peri<'_, _>, found impl PioPin` (×2) | те саме, з іншого боку |

### 3.3. boot2: тепер від embassy-rp

**Що таке boot2.** RP2040 після ввімкнення виконує з внутрішнього ROM код, який завантажує в RAM перші 256 байт зовнішньої flash і запускає їх. Ці 256 байт — boot2 («second stage bootloader»): він налаштовує QSPI під конкретну мікросхему flash (на RP2040-Zero — W25Q080 або сумісна) і вмикає виконання коду прямо з flash (XIP). Без правильного boot2 прошивка не стартує.

**Було.** boot2 додавали ми самі: байти з крейта `rp2040-boot2`, статична змінна в спеціальній секції, і секція в `memory.x`, яка кладе її на адресу `0x10000000`:

```rust
// src/main.rs
#[unsafe(link_section = ".boot_loader")]
#[used]
pub static BOOT2_FIRMWARE: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;
```

```
/* memory.x */
MEMORY {
  BOOT_LOADER : ORIGIN = 0x10000000, LENGTH = 0x100
  FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100
  RAM : ORIGIN = 0x20000000, LENGTH = 264K
}
SECTIONS {
  .boot_loader ORIGIN(BOOT_LOADER) : { KEEP(*(.boot_loader*)); } > BOOT_LOADER
} INSERT BEFORE .text;
```

**Стало.** embassy-rp 0.9 сам містить boot2. Для RP2040 за замовчуванням це W25Q080, а інші мікросхеми обираються через feature `boot2-*`. Він кладе boot2 своїм лінкерним скриптом `link-rp.x` у регіон з іменем `BOOT2`. Тому, як у gabriele-embassy:

1. **`src/main.rs`.** Видалено `BOOT2_FIRMWARE` разом з атрибутами.
2. **`Cargo.toml`.** Видалено `rp2040-boot2`.
3. **`memory.x`.** Регіон `BOOT_LOADER` перейменовано на `BOOT2`, власну секцію `SECTIONS { .boot_loader … }` прибрано:

   ```
   MEMORY {
       /* Second stage bootloader, provided by embassy-rp (boot2-w25q080 by default) */
       BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
       FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100
       RAM   : ORIGIN = 0x20000000, LENGTH = 264K
   }
   ```

4. **`.cargo/config.toml`.** Додано `-Tlink-rp.x` до `rustflags`, бо без нього boot2 з embassy-rp у прошивку не потрапить:

   ```diff
      "-C", "link-arg=-Tlink.x",
   +  "-C", "link-arg=-Tlink-rp.x",
      "-C", "link-arg=-Tdefmt.x",
   ```

**Перевірка.** У заголовках ELF є сегмент на `0x10000000` розміром 256 байт, і починається він з `00 b5 32 4b 21 20 58 60` — ті самі байти, що в стандартному boot2 W25Q080. Прошивка після цього стартує (3.6), тож boot2 правильний.

### 3.4. Тип пінів `Peri`

У embassy-rp 0.9 периферія (`p.PIN_0`, `p.PIO0` тощо) має тип `Peri<'a, T>` — «володіння периферією T на час 'a». Раніше передавали сам `PIN_0`, і функція приймала `impl PioPin`. Тепер `make_pio_pin` хоче саме `Peri`, тож змінилася лише сигнатура нашої функції:

```diff
+use embassy_rp::Peri;
 …
 fn setup_pio_task_sm0<'d>(
     pio: &mut Common<'d, PIO0>,
     sm: &mut StateMachine<'d, PIO0, 0>,
-    dshot_pin: impl PioPin,
-    debug_pin: impl PioPin,
+    dshot_pin: Peri<'d, impl PioPin>,
+    debug_pin: Peri<'d, impl PioPin>,
 ) {
```

Виклик `setup_pio_task_sm0(&mut common, &mut sm0, p.PIN_0, p.PIN_3)` лишився тим самим.

### 3.5. Що не змінилося

Усе інше скомпілювалося без правок:
- PIO: `pio_asm!`, `Config`, `ShiftConfig`, `set_pin_dirs`, side-set, дільник частоти;
- USB: `Driver`, `Builder`, `CdcAcmClass`, `write_packet`;
- executor (`#[embassy_executor::main]`, `join3`), `Timer`, `Instant`, `Mutex`.

Попереджень про застарілі API (deprecated) немає.

### 3.6. Перевірки

| Перевірка | Результат |
|---|---|
| `cargo build`, `cargo build --release` | ✅ |
| `cargo clippy` | ✅ без попереджень |
| `cargo fmt` | ✅ (переставив `use embassy_rp::Peri` за алфавітом) |
| `cargo test-codec` | ✅ 20 тестів |
| розмір у flash | 45 608 байт (було 44 228, +1 380) |
| прошивання `probe-rs run` | ✅ 1,81 с |
| лог старту | ✅ `[INFO ] dshot-reader-embassy: DShot300 on GPIO0, debug on GPIO3 (… src/main.rs:302)` |
| USB | ✅ з'явився `/dev/ttyACM0` з `ID_MODEL=DShot_logger`, `ID_VENDOR_ID=c0de` (проба — `/dev/ttyACM1`, `2e8a`) |
| вміст рядків USB | ⏳ не прочитано: порт належить групі `dialout`, у Claude немає доступу |

FC на лінії не було, тож прийом кадрів, відповідь і EDT не перевірялися.

## 4. Що перевірити на стенді

Найбільший ризик — тихі зміни в обгортці PIO embassy-rp 0.9: розрахунок дільника, конфігурація пінів і side-set, порядок зсуву. Програма PIO та сама, але її налаштування тепер робить інша версія бібліотеки. Повторити перевірки звіту 003:

| Що | Очікується |
|---|---|
| `tio` без FC | `no frames n=0 bad=0 txf=0 late=0 gap=0 edt=0` |
| `tio` з FC | `n` ≈ 2 020 за рядок, `bad` = `txf` = 0, `gap` ≈ 250 мкс |
| `dshot_telemetry_info` | eRPM за моделлю етапу C (газ 1000 → 21 000), Invalid 0 % |
| осцилограф | біт кадру 3,33 мкс, вибірка ≈ 1,7 мкс після спаду (голка GPIO3), пауза ≈ 31 мкс, відповідь 56 мкс |
| EDT (`set dshot_edt = ON`) | `edt=1`, лог `EDT true: reply 0xe02`; Type `RT-C-` |

## 5. Коміти

1. `cargo update`: лише `Cargo.lock`.
2. Міграція embassy 0.9: `Cargo.toml`, `Cargo.lock`, `memory.x`, `.cargo/config.toml`, `src/main.rs` і цей документ.
