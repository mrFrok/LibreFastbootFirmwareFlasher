# Changelog

Each release's section below becomes its GitHub release notes
(`scripts/release-notes.py`); the release fails without one.

## 3.1.0

### Repair system updates

Phones flashed by versions before 3.0 often cannot install system updates
("Attempting to create duplication partition"): those versions left the
inactive slot's partitions in the wrong group. **Tools → Repair updates (OTA)**
fixes it without root: it restarts the phone into fastbootd, removes the
inactive slot's (empty) partitions and restarts it. Each update switches
slot, so run it once before each of the next two updates and both slots are
healed. It refuses devices without Virtual A/B and updates still in progress.

### New

- **Home** is the start page: the connected phone at a glance (model, mode,
  bootloader, slot, processor, battery), the way into installing, the app's
  version and update, and the project links.
- **Rebuild super** (Settings → Experimental, off by default): before
  flashing a payload .zip, replaces super's layout with the one the firmware
  describes — groups and read-only partitions as from the factory, checked
  against a factory OnePlus 15 layout. Refuses when it would drop partitions
  the firmware does not carry (my_company, my_preload on OnePlus).

### Fixes

- A bootloader hidden as locked (GBL exploit, root hiders) no longer blocks
  installing: Android's "locked" is not trusted, fastboot decides.
- GUI builds on release are cached and faster.

---

### Коротко по-русски

- **Починить обновления (OTA)** в «Инструментах»: если после прошивки
  старыми версиями не ставятся обновления системы, инструмент чинит это без
  root. Запустите его перед каждым из двух следующих обновлений.
- **Главная** — стартовая страница: смартфон, версия, ссылки.
- **Пересоздавать super** (Настройки → Экспериментальное, по умолчанию
  выключено): заводская разметка super из прошивки.
- Загрузчик, скрытый эксплойтом GBL, больше не мешает установке.

## 3.0.0

1.x was a Python script, 2.x brought the Rust GUI; 3.0 rebuilds the
interface so that installing firmware takes a few clicks.

### New interface

- **Install firmware** is one page that completes on its own: drivers are
  checked at start, the firmware is unpacked as soon as it is chosen (or
  downloaded from a link, 4PDA links included), the phone is found without
  a button, and its processor is asked once per model and remembered.
- One confirmation before the install: the ARB or preloader warning, then a
  summary where boot, init_boot, recovery, vendor_boot or any partition can
  be left as it is (e.g. to keep root). Then the install runs by itself:
  restart into fastbootd from any mode, cable check, safety checks, flash.
- Clear results: the time it took, a big green check after the restart, and
  plain words when something stops.
- **Tools** holds everything else (one partition, own builds, restart into a
  mode, slot A/B, connection test, drivers, download/unpack, logs); any tool
  can be pinned to the sidebar.
- **Settings**: appearance, language, download folder, unpacked firmware,
  remembered processors, sound notifications.
- The 2.9 interface stays available as **Classic interface** in Settings.

### Safer installs

- A warning when the firmware is built for another device (regional variants
  of one phone pass).
- The computer is kept awake while flashing, and closing the window
  mid-flash asks first.
- A desktop notification with a sound when the install ends.
- Firmware unpacked once is reused; unpacking can be cancelled.
- A web page saved instead of the firmware is caught with a plain message.

### Fixes

- Dynamic partitions keep their group and attributes: flashing no longer
  breaks the next OTA ("Attempting to create duplication partition").
  Thanks to MisterZtr for the report. Phones flashed by an earlier version
  still need a clean super layout; a builder for it is planned.
- 4PDA `stat/go?url=` links download the firmware, not 4PDA's page.
- The window has an app id: Wayland docks show its icon.

### Packaging

- **macOS: the app now comes from a Homebrew cask.** After upgrading, run
  `brew install --cask librefastbootfirmwareflasher`; it installs the app
  into /Applications together with the `lfff` command. The `lfff` formula is
  the command-line tool on macOS, and both on Linux.
- Slint 1.18.1.

---

### Коротко по-русски

1.x — скрипт на Python, 2.x — GUI на Rust, 3.0 — новый интерфейс: прошивка
ставится в несколько кликов.

- **Установка прошивки** — одна страница, шаги отмечаются сами: драйверы,
  прошивка (zip, папка или ссылка, в том числе с 4PDA — распаковывается
  сразу), смартфон (находится сам), процессор (спрашивается один раз для
  модели).
- Одно окно подтверждения: предупреждение ARB или preloader, затем итог, где
  можно не прошивать boot, init_boot, recovery, vendor_boot и другие разделы
  (например, чтобы сохранить root). Дальше установка идёт сама.
- Понятный результат: время прошивки, зелёная галочка после перезагрузки,
  простые слова, если что-то пошло не так.
- **Инструменты** — всё остальное, любой инструмент можно закрепить в
  боковом меню. **Настройки** — внешний вид, язык, папки, звук.
- Старый интерфейс остался: «Классический интерфейс» в настройках.
- Предупреждение, если прошивка от другого смартфона; компьютер не засыпает
  во время прошивки; закрытие окна посреди прошивки спрашивает
  подтверждение; уведомление со звуком в конце.
- Исправлено: разделы в super сохраняют группу, и OTA после прошивки больше
  не падает (спасибо MisterZtr). Смартфонам, прошитым прошлой версией, нужна
  чистая разметка super — сборщик для неё в планах.
- **macOS:** приложение теперь ставится через cask —
  `brew install --cask librefastbootfirmwareflasher`.
