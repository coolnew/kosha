# Install and build on Linux

## Run a release archive

Download the Linux archive for your CPU architecture and its matching
`.sha256` file from [Releases](https://github.com/coolnew/kosha/releases).
In the download directory, verify the checksum before extracting. The
following commands use an example archive name. Substitute the exact name you
downloaded.

```sh
archive=kosha-v0.9.0-x86_64-unknown-linux-gnu.tar.gz
sha256sum -c "$archive.sha256"
tar -xzf "$archive"
cd "${archive%.tar.gz}"
./kosha --version
./kosha-gui --version
./kosha-gui "$HOME/Downloads"
```

`kosha` runs in a terminal, including on a headless machine. `kosha-gui`
needs a graphical desktop and its system libraries. To place both binaries
on your PATH and add a launcher and icon, see
[desktop integration](../../assets/README.md#installing-on-linux).

## Desktop dependencies

- Install the XCB and XKB runtime libraries. Debian/Ubuntu package names are
  `libxcb1`, `libxkbcommon0`, and `libxkbcommon-x11-0`. Arch packages are
  `libxcb`, `libxkbcommon`, and `libxkbcommon-x11`.
- For Wayland, also install the Wayland client runtime (`libwayland-client0`
  on Debian/Ubuntu or `wayland` on Arch). Provide working graphics drivers
  for your desktop. The Vulkan loader is `libvulkan1` on Debian/Ubuntu or
  `vulkan-icd-loader` on Arch. The graphics driver supplies the implementation.
- Font discovery uses Fontconfig. Install `fontconfig` and a desktop font if
  your system has a minimal graphical installation.
- For folder and image pickers and save dialogs, install `xdg-desktop-portal`
  and a backend that supports FileChooser for your desktop. For example,
  `xdg-desktop-portal-gtk` provides the GTK chooser on Xfce or i3.

## Build from source

Install a native C toolchain and Rust 1.90 or later for the CLI. The current
locked Linux GUI dependencies require Rust 1.92 or later. Python 3.11 or later
is needed for the repository's Python development checks. From the source root,
build the CLI alone on a headless machine or include the desktop app.

```sh
cargo build --locked --release -p kosha-cli
cargo build --locked --release -p kosha-cli -p kosha-gui
cargo test --locked --workspace
```

Install the GUI development dependencies before building or testing the full
workspace. The Debian/Ubuntu list follows the project's Linux CI setup.

```sh
sudo apt-get install build-essential pkg-config libwayland-dev libxkbcommon-x11-dev \
  openssl libssl-dev libzstd-dev libvulkan1 libgit2-dev make cmake clang mold
```

On Arch, install the corresponding build dependencies.

```sh
sudo pacman -S --needed base-devel pkgconf wayland libxcb libxkbcommon-x11 \
  openssl zstd vulkan-icd-loader libgit2 cmake clang mold
```

With default Cargo paths, run `./target/release/kosha` or
`./target/release/kosha-gui`. If you set `CARGO_TARGET_DIR`, use its
`release/` subdirectory. To install the source-built CLI for your account,
run `install -Dm755 target/release/kosha "$HOME/.local/bin/kosha"` and make
sure `~/.local/bin` is on your PATH. The desktop integration guide has the
corresponding GUI, icon and launcher commands.

## If launch or a dialog fails

- For a missing shared library, run `ldd ./kosha-gui` and look for `not found`.
  Install the matching runtime package. If the error names an unavailable
  `GLIBC_` version, build the application from source on that system.
- Launch `./kosha-gui` from a terminal in your desktop session to capture
  errors. For renderer failures, check that your display session and graphics
  driver are working. The CLI is available without a graphical session.
- If a picker fails, check the portal and FileChooser backend for the running
  desktop session. On a Linux desktop using a systemd user session, inspect
  the portal with `systemctl --user status xdg-desktop-portal.service` and
  `journalctl --user -u xdg-desktop-portal.service -b`. You can open a folder
  directly with `./kosha-gui /path/to/folder` and export from the CLI with
  `--out` while troubleshooting.

See [stored files](settings-and-files.md#application-directories) and
[troubleshooting scans and settings](troubleshooting.md).
