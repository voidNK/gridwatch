# Changelog

The version is raised with every change that reaches users: the last number for a fix,
the middle number for a new feature. It is set in `Cargo.toml` and shown in the top bar
and by `gridwatch --version`. A tag `v<version>` on GitHub builds and publishes the release.

## 0.10.5

- Fixed: on macOS "About gridwatch" and Finder kept showing the version that was first installed
  after an update from inside gridwatch. The update now writes the new version into the app, and
  an app that was updated before is set right the next time it starts.
- The button that ends a process is labelled Kill.

## 0.10.4

- Fixed: on Windows, notifications appeared under the name "Windows PowerShell". They now carry
  gridwatch's name and icon.
- Windows: a click on a notification brings the gridwatch window to the front, on the server the
  notification was about, with its suggestions open.

## 0.10.3

- Fixed: gridwatch became sluggish and froze for many seconds with a large history. Changing the
  range loads in the background now, and the database averages the samples instead of handing
  every one to the program. The suggestions are worked out once per sample, not for every drawn
  frame. Over a long range the charts keep a fixed number of points instead of growing by the hour.
  Deleting old samples no longer reads the whole database, and no longer blocks the window.
- History of servers that are no longer in the list is removed at start.

## 0.10.2

- Rows can be selected in every table: a click marks a row, another click lets it go. The mark
  follows the row when the table is sorted or refreshed.
- Start, Stop and Restart for containers and End for processes moved out of the table into a bar
  above it, with icons. They act on the selected row and are shown only when allowed in the settings.

## 0.10.1

- **Container actions**: Start, Stop and Restart buttons for docker containers in the Docker tab.
- **End processes**: an End button per process in the Processes tab, with a choice between asking
  the process to end and killing it.
- Both are off until switched on in the settings (new section *Actions*). Stopping, restarting and
  ending ask for confirmation first, and the result is reported at the top of the server page.
- Fixed: `--screenshot` did not end the program when the tray icon was enabled.

## 0.10.0

- When a critical problem newly hits a server, its entry in the server list gets a red frame that
  fades out over 45 seconds, so it catches the eye even when alerts are muted.

## 0.9.3

- Fixed: on macOS the keychain asked for permission once per server at every start (and again
  after each update). All passwords now share one keychain item, so macOS asks once. Passwords
  stored by older versions are moved over the first time they are read.

## 0.9.2

- Fixed: on macOS a click on the tray icon did not bring the window back after it was closed,
  and the menu of the icon did nothing while the window was hidden. gridwatch now hides and shows
  the application the way Mac programs do; a click on the Dock icon brings it back as well.

## 0.9.1

- gridwatch runs only once: starting it again brings the window of the running copy to the front,
  also when it is hidden in the tray.

## 0.9.0

- **Tray icon** (Windows and macOS): gridwatch sits in the notification area or menu bar. The icon
  carries a yellow or red dot when a server has warnings or critical problems, and its tooltip says
  how many. Closing the window hides it there; a click on the icon brings it back. The menu has
  Show, Alerts on/off and Quit. Can be switched off in the settings.

## 0.8.4

- **Installers**: a setup program for Windows (`...-setup.exe`, installs for the current user, with
  Start menu entry and uninstaller) and a disk image for macOS (`.dmg`, drag the app to Applications).
- gridwatch has an icon: in the window, the taskbar, the Windows program file and the macOS app.
- The macOS archive holds only the app again; the bare program was in 0.8.3 for the update from 0.8.1.

## 0.8.3

- Fixed: on macOS the update from 0.8.1 or older failed with "the archive does not contain the
  program", because 0.8.2 moved the program into `gridwatch.app`. The archive now holds the bare
  program next to the app again, where older versions look for it.

## 0.8.2

- **macOS**: releases now contain `gridwatch.app`. Started by double click it opens no Terminal
  window any more. Whoever runs the bare program from an older release has to download this
  version once by hand; the updater keeps an installation in the form it has.

## 0.8.1

- The settings window opens about a fifth taller, so nothing is cut off at the bottom.

## 0.8.0

- **Notifications with colour**: desktop notifications start with a red or yellow mark for the
  severity, and each problem carries a symbol for its topic (disk, temperature, memory, CPU,
  container, GPU, connection). Can be switched off in the settings.
- **Choice of sounds**: warnings and critical problems each get one of five sounds - Chime, Beeps,
  Bell, Sonar or Alarm. The test buttons play the chosen ones.

## 0.7.2

- About: the link to the website is a third large button, with the GitHub mark.

## 0.7.1

- The settings window is wider and has two columns: the settings on the left, About with large
  Ko-fi and PayPal buttons on the right.

## 0.7.0

- The Ko-fi and PayPal buttons moved out of the top bar into a new About section in the settings.
- **Memory tab**: how the memory splits into programs, cache and free, both over time; swap;
  the largest processes; and the finer breakdown the operating system reports.
- **Hardware tab**: what is built into the machine - system and mainboard, processor, memory
  modules, disks, graphics, storage controllers, network adapters and ports. Read once after
  connecting. Memory modules on Linux need `sudo dmidecode` to work without a password.

## 0.6.0

- **Updates**: gridwatch asks GitHub at start whether a newer version exists and shows an
  "Update to ..." button in the top bar. A click shows what is new; "Download and install" fetches
  the release, compares it with its checksum and replaces the program, "Restart now" starts the new
  version. Nothing is installed without a click. The check can be switched off in the settings,
  and `gridwatch --update` does the same from a terminal.

## 0.5.3

- The Ko-fi and PayPal buttons moved from the settings to the top bar, next to Settings, each with an icon.

## 0.5.2

- Settings: a Ko-fi button next to the PayPal one.

## 0.5.1

- Fixed: on macOS a "Choose Application" dialog appeared with the first alert, typically right
  after adding a server.

## 0.5.0

- **Resizable columns**: drag the border between two column titles to make a column wider or
  narrower, in every table. Content that no longer fits is cut off instead of wrapping.
- The server list shows two lines per server: state, name and alerts, then CPU and memory.
- The settings window can be resized and moved.
- Settings has a button to support gridwatch with a donation.
- The top bar is tidier: the word "Range" and the history retention note are gone.

## 0.4.1

- The server list is more compact, so more servers fit without scrolling. The address appears
  when the pointer rests on a server.

## 0.4.0

- The GPU tab now comes right after the overview, before CPU.
- **Sortable tables**: click a column title in the process, container, interface, disk, filesystem,
  sensor and GPU tables to sort by it; a second click reverses, a third returns to the usual order.
  This replaces the CPU / Memory switch above the process list.

## 0.3.0

- **This computer** is listed as the first server: read through the Windows API on Windows,
  through the same scripts as remote machines on Linux and macOS. It can be hidden in the settings.
- **WSL**: the default Linux distribution of a Windows computer can be listed as well (settings, off by default).
- **Network tab**: received and sent traffic for every interface, with charts and totals.
- **Disks tab**: read and write transfer per disk, operations per second and the share of time a disk is busy.
- **Temperatures tab**: every sensor the machine exposes (CPU, GPU, NVMe and SATA disks, mainboard),
  with a chart per chip. On a Mac the values come from `macmon` when it is installed.
- **GPU tab**: load, memory, temperature and power for NVIDIA GPUs (`nvidia-smi`), AMD GPUs (`amdgpu`)
  and Apple Silicon (`macmon`). Shown only on machines that have a GPU.
- **Passwords** and key passphrases can be kept in the keychain of the operating system
  (Windows Credential Manager, macOS Keychain, Secret Service on Linux), so they are asked for only once.
- **Server list**: drag a server to a new place to change the order.
- New suggestions: saturated disk, hot disk, hot GPU, full GPU memory.
- The version number is shown in the top bar; `--version` prints it.
- GitHub Actions build and test on Windows, Linux and macOS, and publish a release for every version tag.
- Fixed: the temperature shown for a machine is now the CPU sensor, not the hottest sensor of any kind
  (mainboard inputs that are not connected often report nonsense).

## 0.2.0

- Alerts: a sound and a desktop notification when a server gets into trouble or cannot be reached,
  with a switch in the top bar.
- Settings window: volume, which problems to alert on, reminders, refresh interval, history retention.
- macOS machines can be monitored, including usage per core.
- Docker is also found in `/usr/local/bin` and `/opt/homebrew/bin`.

## 0.1.0

- First version: servers over SSH with tabs for overview, CPU, cores, processes, docker and suggestions,
  charts from 5 minutes to 7 days, history in SQLite.
