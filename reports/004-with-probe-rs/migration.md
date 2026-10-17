# 004 — прошивання через probe-rs

**Дата:** 2026-10-17. **Гілка:** `with-probe-rs`.

**Мета:** прошивати RP2040 так само, як у проєкті `/home/taras/learn/gabriele-embassy`: через SWD debug-пробу і `probe-rs run`, з логами defmt по RTT. Уся частина gabriele-embassy, пов'язана з WiFi (`cyw43`, `cyw43-pio`, `embassy-net`, прошивка `cyw43-firmware`), сюди **не** переноситься: у нашого RP2040 WiFi немає.

> **Коротко.**
> - `cargo run --release` тепер прошиває плату через пробу (≈ 1,7 с), скидає її і показує логи defmt.
> - Замість `elf2uf2-rs -d` (копіювання UF2 на плату в режимі BOOTSEL) — `probe-rs run --chip RP2040`.
> - Додано `defmt` + `defmt-rtt` для логів і `panic-probe` замість `panic-halt`, щоб паніка друкувалася в лог.
> - Вивід телеметрії по USB (CDC) не змінився.
> - Перевірено на платі: прошивання і лог старту з місцем у коді.

## 1. Вихідний стан

### 1.1. Проба

```
$ probe-rs list
The following debug probes were found:
[0]: Debug Probe (CMSIS-DAP) -- 2e8a:000c-0:E66480454F2FB036 (CMSIS-DAP)
```

`2e8a:000c` — Raspberry Pi Debug Probe (CMSIS-DAP). `probe-rs` — версія 0.32.0.

### 1.2. Як було в нашому проєкті

| Що | Було |
|---|---|
| runner | `elf2uf2-rs -d`: робить UF2 і копіює його на RP2040, змонтований як диск (BOOTSEL) |
| логи | немає; лише текст телеметрії по USB |
| паніка | `panic-halt`: плата мовчки зависає |
| link-args | `--nmagic`, `-Tlink.x` у `rustflags` (`.cargo/config.toml`) |
| boot2 | власний `BOOT2_FIRMWARE` з `rp2040-boot2` і секція `.boot_loader` у `memory.x` |
| embassy | `embassy-rp` 0.4, `embassy-executor` 0.7, `embassy-time` 0.4 |

### 1.3. Як у gabriele-embassy

| Що | gabriele-embassy |
|---|---|
| runner | `probe-rs run --chip RP2040` |
| логи | `defmt` 1.0 + `defmt-rtt` 1.0, `DEFMT_LOG = "debug"` у `[env]` |
| паніка | `panic-probe` 1.0 з `print-defmt` |
| link-args | у `build.rs`: `--nmagic`, `-Tlink.x`, `-Tlink-rp.x`, `-Tdefmt.x` |
| boot2 | від `embassy-rp` (`-Tlink-rp.x`), `memory.x` з `BOOT2` |
| embassy | `embassy-rp` 0.9, `embassy-executor` 0.9, `embassy-time` 0.5 |
| WiFi | `cyw43`, `cyw43-pio`, `embassy-net` — **не переносимо** |

### 1.4. Що беремо, а що ні

| Береться | Не береться і чому |
|---|---|
| runner `probe-rs run --chip RP2040` | — |
| `defmt` + `defmt-rtt` | — |
| `panic-probe` з `print-defmt` | — |
| `-Tdefmt.x` | — |
| `[env] DEFMT_LOG` | — (рівень `info`, а не `debug`: логів у нас мало) |
| — | `build.rs` з link-args: у нас ті самі link-args уже задані в `rustflags`, переносити їх немає сенсу |
| — | `-Tlink-rp.x` і `memory.x` з `BOOT2`: це для boot2 з `embassy-rp` 0.9; у нас власний boot2, який працює з 0.4 |
| — | оновлення embassy до 0.9: велика окрема зміна (інші API), для прошивання через probe-rs не потрібна |
| — | `cyw43*`, `embassy-net`: WiFi немає |

## 2. Кроки

### Крок 1. Runner і link-args (`.cargo/config.toml`)

```diff
 rustflags = [
   "-C", "link-arg=--nmagic",
   "-C", "link-arg=-Tlink.x",
+  "-C", "link-arg=-Tdefmt.x",
   "-C", "no-vectorize-loops",
 ]

-runner = "elf2uf2-rs -d"
+runner = "probe-rs run --chip RP2040"

-# runner = "probe-rs run --chip RP2040"
+# runner = "elf2uf2-rs -d"
+
+[env]
+DEFMT_LOG = "info"
```

- **`runner`.** `cargo run` викликає `probe-rs run --chip RP2040 <ELF>`. probe-rs знаходить пробу, прошиває flash через SWD, скидає чип і лишається під'єднаним, щоб читати RTT. Старий runner лишився закоментованим для роботи без проби.
- **`-Tdefmt.x`.** Лінкерний скрипт defmt. defmt не зберігає рядки логів у прошивці: у flash іде лише індекс, а самі рядки — в окрему секцію ELF, яку читає probe-rs. Без цього скрипта лінкування з defmt не проходить.
- **`DEFMT_LOG = "info"`.** Рівень логів, що вкомпільовуються: `info!` і вище. `debug!` і `trace!` викидаються ще під час компіляції.

Alias `test-codec` не змінився і далі працює: тести кодека на ПК не залежать від defmt.

### Крок 2. Залежності (`Cargo.toml`)

```diff
 cortex-m-rt = "0.7.5"
+defmt = "1.0.1"
+defmt-rtt = "1.0.0"
 dshot-codec = { path = "dshot-codec" }
 …
-panic-halt = "1.0.0"
+panic-probe = { version = "1.0.0", features = ["print-defmt"] }
 rp2040-boot2 = "0.3.0"
```

| Крейт | Навіщо |
|---|---|
| `defmt` | макроси логування `defmt::info!` тощо: компактний бінарний формат замість тексту |
| `defmt-rtt` | транспорт: пише логи в буфер RTT в RAM, звідки probe-rs читає їх через SWD, не зупиняючи процесор |
| `panic-probe` (`print-defmt`) | при паніці друкує повідомлення через defmt і зупиняє процесор так, що probe-rs це бачить і завершується з помилкою |

Cargo підтягнув з crates.io: `defmt` 1.2.0, `defmt-macros` 1.2.0, `defmt-parser` 1.1.0, `defmt-rtt` 1.3.1, `panic-probe` 1.0.0 і їхні залежності (`thiserror`, `syn`, `bitflags`). `panic-halt` з `Cargo.lock` зник.

Версії embassy (`embassy-rp` 0.4 і решта) **не змінювалися**. Їм defmt не потрібен: ми не вмикаємо в них feature `defmt`.

### Крок 3. Код (`src/main.rs`)

```diff
-use panic_halt as _;
+use {defmt_rtt as _, panic_probe as _};
```

`defmt_rtt` і `panic_probe` нічого не експортують, що ми викликали б. Їх треба лише підключити, щоб вони зареєстрували транспорт логів і обробник паніки.

Два логи:

```rust
// у main, одразу після embassy_rp::init:
defmt::info!("dshot-reader-embassy: DShot300 on GPIO0, debug on GPIO3");

// у задачі PIO, коли EdtSwitch увімкнув або вимкнув EDT:
defmt::info!("EDT {=bool}: reply {=u16:#x}", edt.enabled(), e12);
```

Другий лог — у гарячому циклі задачі PIO, але спрацьовує лише при зміні стану EDT, тобто рідко. `defmt-rtt` за замовчуванням не блокує: якщо буфер RTT повний (проба не читає), запис просто відкидається, і задача PIO не чекає.

### Крок 4. Перша спроба прошити — і проблема з процесом

Перша спроба була така:

```
timeout 25 cargo run --release 2>&1 | tail -20
```

Що пішло не так:
- **`probe-rs run` не завершується сам:** після прошивання він читає RTT, доки його не зупинять Ctrl-C.
- **`timeout` зупинив `cargo`, але не дочірній `probe-rs`.** `probe-rs` лишився працювати і тримав пробу.
- **`| tail` буферизує все до кінця потоку,** тож вивід першої спроби загубився.
- **Друга спроба отримала помилку:**

  ```
  ERROR nusb::error: interface is busy (errno 16)
  Error: Failed to open probe: Failed to open the debug probe.
  ```

  Проба зайнята першим `probe-rs`.

**Як виправлено:**
1. Зависший `probe-rs` (на SIGINT і SIGTERM не відповідав) зупинено `kill -KILL`, і проба звільнилася.
2. Для перевірки `probe-rs` запускався напряму, без `cargo`, і зупинявся сигналом SIGINT, як від Ctrl-C:

   ```
   timeout -s INT -k 5 20 probe-rs run --chip RP2040 target/thumbv6m-none-eabi/release/dshot-reader-embassy > probe-run.log 2>&1
   ```

   `-s INT` дає probe-rs коректно від'єднатися від чипа, а `-k 5` гарантує, що через 5 с процес буде вбито, навіть якщо він не відповідає. Вивід іде у файл, тож нічого не буферизується.

Для звичайної роботи нічого з цього не треба: `cargo run --release` у терміналі і Ctrl-C, коли досить.

### Крок 5. Прошивання і перший лог

```
     Finished in 1.71s
[INFO ] dshot-reader-embassy: DShot300 on GPIO0, debug on GPIO3 (<crate> └─ <invalid location: defmt frame-index: 3>:0)
```

і попередження:

```
WARN probe_rs::util::rtt::processing: Insufficient DWARF info; compile your program with `debug = 2` to enable location info.
```

Прошивання працює, лог з плати приходить. Але місце в коді не показується, бо в release-збірці немає повної debug-інформації.

### Крок 6. Debug-інформація для release (`Cargo.toml`)

```toml
[profile.release]
# Full debug info for probe-rs: defmt log locations. It stays in the ELF, not in flash.
debug = 2
```

Debug-інформація лишається в ELF на ПК і на прошивку не впливає. Розмір того, що потрапляє у flash, порахований із заголовків ELF: **44 228 байт** (boot2 256 + вектори 192 + код 34 484 + дані 8 712 + решта). Сам ELF виріс до 2,6 МБ, але на плату йде лише flash-частина.

Повторне прошивання:

```
     Finished in 1.69s
[INFO ] dshot-reader-embassy: DShot300 on GPIO0, debug on GPIO3 (dshot_reader_embassy dshot-reader-embassy/src/main.rs:305)
Received Ctrl+C, exiting
Exited by user request
```

Тепер видно файл і рядок. Після Ctrl-C probe-rs від'єднується, а прошивка працює далі (обидва порти `/dev/ttyACM*` на місці).

Логу `EDT …` не було, і так і мало бути: FC цього дня до лінії не під'єднували, тож кадрів, зокрема команди 13, не було взагалі.

### Крок 7. Перевірки

| Перевірка | Результат |
|---|---|
| `cargo build` (debug) | ✅ |
| `cargo build --release` | ✅ |
| `cargo clippy` | ✅ без попереджень |
| `cargo fmt --check` | ✅ |
| `cargo test-codec` | ✅ 20 тестів |
| прошивання `probe-rs run` | ✅ ≈ 1,7 с |
| лог defmt з місцем у коді | ✅ |

### Крок 8. Документи

- **`README.md`, розділ «Installation».** Прошивання через пробу (`cargo run --release`), що показує `probe-rs run`, наголос на release-збірці (debug затримує задачу PIO на ≈ 1,7 мс, `reports/003-day-three/findings.md`) і варіант без проби (`elf2uf2-rs -d`).
- **`docs/action-plan.md`.** Два рядки про прошивання: замість «`elf2uf2-rs -d` з платою в BOOTSEL» — «`cargo run --release` через debug-пробу; без проби — `elf2uf2-rs -d`».

## 3. Як тепер працювати

| Задача | Команда |
|---|---|
| перевірити, що пробу видно | `probe-rs list` |
| прошити й дивитися логи | `cargo run --release`, зупинити Ctrl-C |
| прошити без проби | плата в BOOTSEL, у `.cargo/config.toml` — `runner = "elf2uf2-rs -d"`, потім `cargo run --release` |
| телеметрія по USB, як і раніше | `tio /dev/ttyACM…` (порт RP2040 з VID `c0de`) |
| тести кодека на ПК | `cargo test-codec` |

**Що треба пам'ятати:**
- **Лише одна програма може тримати пробу.** Якщо `probe-rs` пише «interface is busy», десь лишився інший `probe-rs` (`pgrep -a probe-rs`).
- **Прошивати release.** Debug-збірка повертає затримки задачі PIO (звіт 003).
- **Логи defmt видно лише з пробою.** Телеметрія по USB від проби не залежить.

## 4. Що не робилося

- Оновлення embassy до версій gabriele-embassy (0.9).
- Перехід на `build.rs` і boot2 від `embassy-rp`.
- Більше логів defmt (наприклад, періодичної статистики): поки що USB-рядок покриває це.
