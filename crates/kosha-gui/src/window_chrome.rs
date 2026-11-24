// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Minimize and close use application tooltips. Maximize/restore retains the
//! native Windows caption hit region for the system's Snap Layouts flyout.

use crate::theme::Palette;
use gpui_kit::{AnyElement, App, IntoElement, Window};

pub fn title_bar(
    content: impl IntoElement,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    #[cfg(windows)]
    {
        windows::render(content, palette, window, cx)
    }
    #[cfg(not(windows))]
    {
        use gpui_kit::{ParentElement, Styled};
        let _ = (palette, window, cx);
        gpui_kit::component::TitleBar::new()
            .h(crate::theme::metrics::scaled(
                crate::theme::metrics::TITLE_BAR_HEIGHT,
            ))
            .child(content)
            .into_any_element()
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::theme::metrics;
    use crate::ui::tooltip;
    use gpui_kit::base::h_flex;
    use gpui_kit::component::{Icon, IconName, InteractiveElementExt as _};
    use gpui_kit::prelude::FluentBuilder;
    use gpui_kit::{
        InteractiveElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, div, px,
    };

    pub fn render(
        content: impl IntoElement,
        palette: &Palette,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let _ = cx;
        let maximized = window.is_maximized();
        let supported = window.window_controls();
        let controls = h_flex()
            .h_full()
            .flex_none()
            .when(supported.minimize, |view| {
                view.child(
                    control(
                        "window-minimize",
                        "Minimize",
                        IconName::WindowMinimize,
                        palette,
                    )
                    .on_click(|_, window, cx| {
                        cx.stop_propagation();
                        window.minimize_window();
                    }),
                )
            })
            .when(supported.maximize, |view| {
                view.child(control(
                    "window-maximize",
                    if maximized { "Restore" } else { "Maximize" },
                    if maximized {
                        IconName::WindowRestore
                    } else {
                        IconName::WindowMaximize
                    },
                    palette,
                ))
            })
            .child(close_control(palette));

        h_flex()
            .id("kosha-title-bar")
            .h(crate::theme::metrics::scaled(metrics::TITLE_BAR_HEIGHT))
            .flex_none()
            .items_center()
            .bg(palette.surface)
            .border_b_1()
            .border_color(palette.hairline)
            // Native caption handling owns drag and double-click. On Windows,
            // zoom_window only maximizes; using it as a toggle prevents restore.
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .window_control_area(gpui_kit::WindowControlArea::Drag)
                    .child(content),
            )
            .child(controls)
            .into_any_element()
    }

    fn close_control(palette: &Palette) -> gpui_kit::Stateful<gpui_kit::Div> {
        control("window-close", "Close", IconName::WindowClose, palette).on_click(
            |_, window, cx| {
                cx.stop_propagation();
                window.remove_window();
            },
        )
    }

    fn control(
        id: &'static str,
        label: &'static str,
        icon: IconName,
        palette: &Palette,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let is_close = matches!(&icon, IconName::WindowClose);
        let native_maximize = matches!(&icon, IconName::WindowMaximize | IconName::WindowRestore);
        h_flex()
            .id(id)
            .role(gpui_kit::Role::Button)
            .aria_label(label)
            .h_full()
            .w(px(34.0))
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(palette.text_dim)
            // GPUI permits one hover handler per element. Choose the close
            // button's colors here rather than appending another handler.
            .hover(|style| {
                if is_close {
                    style.bg(palette.danger).text_color(palette.accent_text)
                } else {
                    style.bg(palette.hover).text_color(palette.text)
                }
            })
            // Windows owns maximize/restore clicks and hover so Snap Layouts
            // works as it did before the other caption tooltips were changed.
            .when(native_maximize, |view| {
                view.window_control_area(gpui_kit::WindowControlArea::Max)
            })
            .when(!native_maximize, |view| {
                view.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_double_click(|_, _, cx| cx.stop_propagation())
                    .tooltip(move |window, cx| tooltip(label).build(window, cx))
            })
            .child(Icon::new(icon).size(px(metrics::TITLE_ICON)))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn caption_controls_construct_without_duplicate_hover_styles() {
            for palette in [Palette::dark(), Palette::light()] {
                // Exercise the actual close-button builder: duplicate hover
                // handlers panic during construction, before a window opens.
                let _close = close_control(&palette);
                for (label, icon) in [
                    ("Minimize", IconName::WindowMinimize),
                    ("Maximize", IconName::WindowMaximize),
                    ("Restore", IconName::WindowRestore),
                ] {
                    let _control = control("test-caption", label, icon, &palette);
                }
            }
        }
    }
}
