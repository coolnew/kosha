<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors -->

# Icon and desktop entry

| File | Used by |
| --- | --- |
| `kosha.ico` | the Windows executable, compiled in by `crates/kosha-gui/build.rs` |
| `kosha.icns` | a macOS `.app` bundle |
| `kosha-256.png`, `kosha-512.png` | Linux icon themes, and anywhere a plain image is wanted |
| `app.coolnew.kosha.desktop` | Linux desktop environments |

## The picture

A squarified treemap in kosha's own palette, because that is what the
application draws — an abstract mark would have had to be justified, and this
one only has to be recognised. The layout comes from the same
Bruls/Huizing/van Wijk algorithm the treemap uses, over a fixed set of weights
chosen to give one dominant tile and a descending tail, which is the shape of a
directory worth opening kosha to look at.

`python xtask-icon.py` redraws every file, deterministically, from the standard
library alone. Replacing this with commissioned artwork means dropping new PNGs
in and rerunning the two container steps; nothing downstream knows how the
pixels were made.

## Installing on Linux

The GUI uses XDG Desktop Portal for opening folders and disk images and saving
exports. Install `xdg-desktop-portal` and a backend that supports the FileChooser
interface for your desktop session, such as `xdg-desktop-portal-gtk` for Xfce.

The desktop entry's `Icon` and `StartupWMClass` both name `app.coolnew.kosha`,
which is the `app_id` the window sets. All three have to agree or the
compositor cannot match the window to the entry, and the result is a running
application with a placeholder icon and no name in the switcher.

The entry's `Exec` names `kosha-gui`, which is the window's binary. `kosha` is
the command-line scanner and is not what a desktop entry should launch — it
would open nothing and exit.

Run this from the directory you unpacked the release archive into, where all
four files sit together. From a clone, the two assets are under `assets/`.

```sh
install -Dm755 kosha-gui ~/.local/bin/kosha-gui
install -Dm755 kosha     ~/.local/bin/kosha
install -Dm644 kosha-256.png \
    ~/.local/share/icons/hicolor/256x256/apps/app.coolnew.kosha.png
install -Dm644 app.coolnew.kosha.desktop \
    ~/.local/share/applications/app.coolnew.kosha.desktop
```

The icon is installed **under the `app_id`**, not under its own filename: the
entry names `app.coolnew.kosha`, and an icon theme lookup is by name, so a file
left as `kosha-256.png` is an icon no compositor will ever find.

`Categories` names exactly one main category — `System` — with `Filesystem` as
the additional one that qualifies it. Two main categories is not an error, and
`desktop-file-validate` warns about it: an application in two of them can
appear twice in the menu.
