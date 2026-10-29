# Changelog

Each release's section below becomes its GitHub release notes
(`scripts/release-notes.py`); the release fails without one.

## 3.5.1

### Fixes

- A flash into a slot that keeps the layout after a super rebuild now
  writes the empty factory images of `my_company` and `my_preload` there
  too. Before, they stayed empty and the phone did not boot from that slot
  (seen on a Realme GT Neo 5 on the second flash after a rebuild).
- "Stop" works again while fastboot waits for a phone that dropped off USB,
  also on boot, abl and the other critical partitions; mid-write those are
  still never stopped. Before, the button did nothing for up to 8 minutes.
- The computer is kept awake through the desktop (xdg-desktop-portal or
  PowerManagement) where there is no systemd or elogind, and the log says
  whether sleep is blocked.
- The update dialog shows the release notes as readable text in a
  scrolling box of proper size, not one line of raw Markdown.

## 3.5.0

### Super partition

- When the slot a Snapdragon flash switches to has no partitions (Virtual
  A/B after an update, or deleted by an older LFFF), super is rebuilt from
  the firmware's layout: every partition read-only in its group, as from
  the factory. Before, fastbootd recreated them outside their group, and
  system updates failed afterwards. On by default, in the app (Settings →
  Super partition) and in `lfff flash` (`--no-rebuild-super` turns it off).
- `my_company` and `my_preload`, which some regional firmware leaves out,
  get empty factory images instead of being lost (images from Neferon).
  `system_dlkm_oki`, the kernel modules of OxygenOS 16's own kernel that
  OnePlus 15 keeps from the factory and nothing loads since OxygenOS 17, is
  dropped. Any other partition a firmware lacks keeps the old way, with the
  warning below.
- Works on OnePlus 15, whose fastbootd does not list the size of super and
  refuses to give it for a second or two after coming from Android.
- `lfff extract` saves the firmware's partition layout too, so the command
  line can rebuild super as well.

### Command line

- stdout now carries only the result: `download` prints the saved file's
  path, `extract` the folder it made, `devices` one `MODE<TAB>SERIAL` line
  per phone, `extract --list` the entries. Progress, reports, questions and
  errors go to stderr, so `lfff extract "$(lfff download URL)"` works.
  Scripts that read the old stdout need updating.
- Usage errors exit with 2 (`lfff arb` with nothing to read, `lfff flash`
  without a folder, `--channel AB`).
- `flash` without a terminal stops at once instead of asking forever on a
  closed stdin; `extract` without `-o` takes the default folder silently.
- `lfff reboot [system|recovery|bootloader|fastbootd]` and
  `lfff set-slot <a|b>`, which used to be GUI-only.
- `download --md5 <HASH>` checks the archive; `find-ota` prints the hash.
  `extract --checksum` accepts a hash in upper case.

### Fixes

- Reboot buttons in the app now target the selected phone; with two phones
  connected, fastboot rebooted whichever it found first.
- The app no longer hangs for good when the phone is unplugged just before
  a slot switch, a reboot or a wipe: every adb/fastboot call has a timeout.
- A firmware downloaded from the update search is checked against the
  server's MD5, and a damaged one is removed rather than reused.
- Preparing super no longer mistakes a fastboot that did not answer for a
  missing partition (which logged a false warning and skipped the resize).
- A damaged `xbl_config.img` is reported as unknown ARB instead of crashing.
- A partial `extract --partitions` is no longer taken by the app for a full
  extraction of that archive.
- The device poller no longer runs fastboot at the same moment the app
  starts working with the phone.
- When super is not rebuilt and the new slot's partitions are missing, the
  log says so once instead of a warning per partition, and the end of the
  flash tells you to run "Repair updates (OTA)" before each of the next two
  system updates.

## 3.4.0

### Flashing

- Snapdragon phones are flashed entirely in fastbootd: the phone switches
  to the other slot, and the modem and the system go there. There is no
  restart into the bootloader any more — some phones cannot reach it, and a
  flash stopped there could brick them. A flash with a modem but no system
  is refused instead of being sent to the bootloader.
- The old way, with the modem through the bootloader, stays as an explicit,
  not recommended choice: `--method snapdragon-bootloader`, the last card
  of the method dialog, or Settings → Firmware → Modem via bootloader.
- Partitions you choose not to flash (a rooted `init_boot`, a custom
  recovery) are copied from the phone over adb as root before the slot
  switch and written to the new slot, so root survives. If they cannot be
  copied, nothing is flashed.

### Anti-rollback

- The ARB installed on the phone is read from its `xbl_config` over adb as
  root (Magisk: allow the prompt; KernelSU / APatch: grant root to Shell).
  `lfff arb --device` shows it, and compares it with `--firmware-dir`.
- The install compares it with the firmware's: the same ARB needs no
  warning, a higher firmware ARB is warned about with both values, and a
  phone ARB above the firmware's refuses the install. When the phone's ARB
  cannot be read, the ARB step says why.
- `lfff arb` no longer reports a firmware ARB it could not read as 0.

### Interface

- A firmware archive the app downloaded is deleted once unpacked; a .zip
  you picked yourself too, with Settings → Firmware → Delete picked .zip.
- A reworked About page, and links to the LFFF Telegram group and to the
  Realme and OnePlus communities.
- The "Fastbootd not confirmed" dialog is translated; an empty log says so
  instead of showing a dash; History uses its icon instead of an emoji.

---

### Коротко по-русски

- Смартфоны на Snapdragon прошиваются целиком в fastbootd: смартфон
  переключается на другой слот, modem и система ставятся туда. Перезагрузки
  в bootloader больше нет — на некоторых смартфонах он недоступен, и
  прошивка через него могла закончиться «кирпичом».
- Старый способ с modem через bootloader остался как явный выбор с
  пометкой «не рекомендуется».
- Разделы, которые вы не прошиваете (пропатченный `init_boot`, свой
  recovery), копируются со смартфона по adb с root и переносятся на новый
  слот — root не слетает.
- ARB смартфона проверяется перед установкой: одинаковый — без
  предупреждения, у прошивки выше — предупреждение, у смартфона выше —
  установка запрещена. Нужен root для Shell (KernelSU / APatch) или
  разрешение в Magisk.
- Скачанные архивы удаляются после распаковки. Новая вкладка «О
  программе», ссылки на группу LFFF и сообщества Realme и OnePlus.

## 3.3.2

### Fixes

- A phone that does not report its bootloader lock state in fastbootd
  (OnePlus Nord 5) is no longer stopped as "locked": the state is asked on
  its own, and only an explicit "locked" stops the install (#16).
- When the phone stops answering during the cable check, the install page
  says so instead of calling the connection too slow, and no longer
  recommends the cable from the box: OPPO / OnePlus / realme fast-charging
  cables are often poor at data — use a plain USB data cable.

---

### Коротко по-русски

- Смартфон, который в fastbootd не сообщает состояние загрузчика (OnePlus
  Nord 5), больше не считается заблокированным: установку останавливает
  только явное «заблокирован».
- Если смартфон перестал отвечать при проверке кабеля, так и пишется —
  а не «соединение слишком медленное». Родной зарядный кабель больше не
  советуется: нужен обычный кабель для передачи данных.

## 3.3.1

### Firmware search

- Finds a new Android version as soon as it is out: it is offered only as
  an update from the newest build of the one before, so the search now
  asks again from there (OnePlus 15 India gets Android 17). Builds offered
  only as early access are marked beta.
- An update server that hangs no longer slows the search down to minutes:
  it is left out and the others answer.

---

### Коротко по-русски

- Поиск прошивки находит новую версию Android сразу после выхода
  (например, Android 17 для OnePlus 15 India). Сборки раннего доступа
  отмечены «бета».
- Зависший сервер обновлений больше не растягивает поиск на минуты.

## 3.3.0

### Find official firmware

**Install firmware → Firmware** can now ask the OPPO / OnePlus / realme
update servers for the phone's firmware: the model is filled in from the
connected phone, Find lists the builds of every region (newest first, with
region, date, size, security patch and what's new), and Download fetches
and unpacks the chosen one. All 19 regions are asked, under each name the
region's phones use (RMX3709TR, RMX3709EX…), in about 8 seconds. The page
says which region the phone's firmware is from and lists that region's
builds first, marked "your region"; the build the phone runs is marked
installed. A package already in the download folder is opened instead of
downloaded again.

On the command line:

```
lfff find-ota CPH2747                       # every region
lfff find-ota CPH2747 -r EU --changes       # one region, with what's new
lfff find-ota CPH2747 --latest | lfff download -
```

The update-server protocol is ported from Houvven's OplusUpdater as kept
current in the RealmeOS/OplusUpdater-android fork (GPL-3.0); how regions
are combined and answers read follows NeFeroN's LinkResolverDownloader.

### Downloads

- OPPO's signed download links live half an hour. A download that takes
  longer now gets a fresh link and continues where it stopped.
- Under the link field, and while downloading: how long the link still
  works, in amber under five minutes, red once expired.
- Downloading a package again no longer reports the first copy as the
  result, and a finished file is never continued into.

---

### Коротко по-русски

- **Поиск официальной прошивки** в шаге «Прошивка»: модель берётся со
  смартфона, «Найти» за ~8 секунд показывает сборки всех 19 регионов — с
  регионом, датой, размером, патчем и списком изменений. Видно, какой
  регион сейчас на смартфоне: сборки этого региона идут первыми с меткой
  «ваш регион», установленная отмечена. «Скачать» скачивает и распаковывает; если пакет уже
  скачан, он открывается сразу. В командной строке — `lfff find-ota`.
- **Ссылки OPPO** живут полчаса: если загрузка дольше, ссылка обновляется
  сама и загрузка продолжается с того же места.
- Под полем ссылки и во время загрузки — сколько ещё живёт ссылка.

## 3.2.0

### Safer super rebuild (experimental)

Rebuild super (Settings → Experimental) now checks everything before it
touches the phone:

- the firmware's partitions must fit their group on this phone, from the
  image sizes in the payload;
- each partition name may appear only once in the whole layout;
- the built layout is read back — checksums and contents — and compared
  with the plan before it is written;
- the install summary shows the layout that will be written, and Install
  stays off until "I understand: rebuild super" is ticked.

The rules follow MisterZtr's analysis of super on Realme / OnePlus / OPPO
devices, which confirms the layout already in use: groups 4 MiB smaller
than super, 3 metadata slots of 64 KiB, the Virtual A/B flag.

---

### Коротко по-русски

Пересоздание super (Настройки → Экспериментальное) теперь до записи
проверяет, что прошивка влезает, что имена разделов не повторяются, и сверяет
собранную разметку с планом. Перед установкой показывается, какая разметка
будет записана, — нужна отдельная галочка «Понимаю: пересоздать super».
Правила сверены с разбором MisterZtr.

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
