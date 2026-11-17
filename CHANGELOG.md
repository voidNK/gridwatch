# Changelog

The version is raised with every change that reaches users: the last number for a fix,
the middle number for a new feature. It is set in `Cargo.toml` and shown in the top bar
and by `gridwatch --version`. A tag `v<version>` on GitHub builds and publishes the release.

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
