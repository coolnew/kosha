// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Native application actions, registered before the first window opens.

use gpui_kit::{App, KeyBinding, Menu, MenuItem, SystemMenuType};

gpui_kit::actions!(kosha, [Quit]);

pub(crate) fn init(cx: &mut App) {
    // Compile the menu against GPUI on every host, but leave Windows/Linux
    // controls alone. Mac menu actions must work even with no focused view.
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
    cx.set_menus([Menu::new("Kosha").items([
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Quit Kosha", Quit),
    ])]);
}
