// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Searchable settings. Filtering changes presentation only; edits keep their
//! existing persistence and scan options still require an explicit rescan.

use gpui_kit::base::input::InputEditorStyle;
use gpui_kit::base::{Input, h_flex, v_flex};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, Focusable, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, relative,
};

use crate::app::KoshaApp;
use crate::settings::{
    AccentSource, Settings, TEXT_SCALE_MAX, TEXT_SCALE_MIN, ThemePreference, UnitPreference,
};
use crate::theme::{Palette, metrics};
use crate::ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Display,
    Scanning,
    Saved,
    Pinned,
    Advanced,
}
impl Group {
    const ALL: [Self; 5] = [
        Self::Display,
        Self::Scanning,
        Self::Saved,
        Self::Pinned,
        Self::Advanced,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Display => "DISPLAY",
            Self::Scanning => "SCANNING",
            Self::Saved => "SAVED SEARCHES",
            Self::Pinned => "PINNED FOLDERS",
            Self::Advanced => "EVERYTHING ELSE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OptionId {
    Units,
    Theme,
    TextSize,
    Accent,
    Tint,
    Cross,
    Exact,
    Hardlinks,
    Mft,
    Image,
    NativeWsl,
    Owners,
    SshAccess,
    DuplicateMin,
    DuplicateBudget,
    Rescan,
    File,
    Version,
    Updates,
}
#[derive(Clone, Copy)]
enum Availability {
    Always,
    Cross,
    Exact,
    Hardlinks,
    Windows,
}
impl Availability {
    fn available(self) -> bool {
        match self {
            Self::Always => true,
            Self::Cross => kosha_core::can_cross_filesystems(),
            Self::Exact => kosha_core::exact_size_costs_extra(),
            Self::Windows => cfg!(windows),
            Self::Hardlinks => kosha_core::hardlinks_cost_extra(),
        }
    }
}

struct OptionSpec {
    id: OptionId,
    group: Group,
    label: &'static str,
    description: &'static str,
    keywords: &'static str,
    availability: Availability,
}

// Search and rendering read the same labels and explanations, so a phrase a
// person has just read remains findable after the wording changes.
const OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        id: OptionId::Updates,
        group: Group::Advanced,
        label: "Software updates",
        description: "Kosha checks GitHub at most once every 24 hours. Install updates using the channel you already use.",
        keywords: "check download release upgrade installer",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::TextSize,
        group: Group::Display,
        label: "Text size",
        description: "Resize desktop text immediately. System default uses the display's normal scaling.",
        keywords: "accessibility zoom font dpi larger smaller zoom percentage",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::Units,
        group: Group::Display,
        label: "Units",
        description: "GiB counts in powers of 1024; GB counts in powers of 1000.",
        keywords: "binary decimal bytes kib mib gib kilobytes megabytes gigabytes",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::Theme,
        group: Group::Display,
        label: "Theme",
        description: "Follow the system appearance, or choose light or dark.",
        keywords: "appearance color colour night bright",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::Accent,
        group: Group::Display,
        label: "Accent",
        description: "Follow the desktop accent colour, or use kosha's. Red still means a destructive action; the treemap keeps its category colours.",
        keywords: "system kosha color colour desktop highlight",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::Tint,
        group: Group::Display,
        label: "Tint the window with the accent",
        description: "Shift the greys toward the accent hue while keeping their brightness and contrast. Dark stays dark and light stays light.",
        keywords: "chrome gray grey appearance color colour",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::Cross,
        group: Group::Scanning,
        label: "Cross filesystem boundaries",
        description: "Off by default so a drive scan does not wander onto network shares. Mounted volumes are crossed; links never are.",
        keywords: "mount network share volume filesystem boundary symlink",
        availability: Availability::Cross,
    },
    OptionSpec {
        id: OptionId::Exact,
        group: Group::Scanning,
        label: "Exact size on disk",
        description: "Accurate for compressed and sparse files, at roughly an order of magnitude in scan time.",
        keywords: "allocated allocation compression ntfs accuracy slow performance",
        availability: Availability::Exact,
    },
    OptionSpec {
        id: OptionId::Hardlinks,
        group: Group::Scanning,
        label: "Count hardlinks once",
        description: "While walking folders, open each file to read its identity. Several times slower, but useful for build directories. Master File Table and disk image scans already count each hardlink once.",
        keywords: "hard links duplicates deduplication identity inode build performance",
        availability: Availability::Hardlinks,
    },
    OptionSpec {
        id: OptionId::Mft,
        group: Group::Scanning,
        label: "Read the Master File Table",
        description: "Read an NTFS volume's index directly for a fast, exact drive scan, including folders this window otherwise cannot read. Needs administrator; otherwise the ordinary scan runs and says so.",
        keywords: "mft ntfs admin elevated elevation index compressed sparse permissions performance",
        availability: Availability::Exact,
    },
    OptionSpec {
        id: OptionId::Image,
        group: Group::Scanning,
        label: "Read disk images",
        description: "Open a backup, exported Linux distribution, whole disk or partition image directly, with or without VHDX. No mount or administrator needed. Supports ext2, ext3, ext4, NTFS, XFS, FAT, exFAT, ISO 9660 and UDF.",
        keywords: "wsl linux distro distribution ext4 vhd vhdx backup image iso udf partition shift",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::NativeWsl,
        group: Group::Scanning,
        label: "Scan inside WSL2",
        description: "Use the matching Linux Kosha CLI installed in the selected distribution. Stay on the Linux root filesystem, skip /mnt, and browse the result read-only. A disk image request takes precedence.",
        keywords: "wsl linux distro distribution native performance home",
        availability: Availability::Windows,
    },
    OptionSpec {
        id: OptionId::Owners,
        group: Group::Scanning,
        label: "Record who owns each file",
        description: "Show who is using the disk on a shared machine. The next scan records each file's owner.",
        keywords: "owner owners ownership user users account accounts uid sid permissions shared disk usage",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::SshAccess,
        group: Group::Scanning,
        label: "SSH access",
        description: "Allow scans of remote folders over SSH. Off by default.",
        keywords: "remote network secure shell connection",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::DuplicateMin,
        group: Group::Scanning,
        label: "Duplicate minimum file size",
        description: "Files below this size are skipped by the duplicate hunt.",
        keywords: "duplicates minimum size threshold",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::DuplicateBudget,
        group: Group::Scanning,
        label: "Duplicate read budget",
        description: "Estimates bytes for hashing. The final same-size group may exceed the budget.",
        keywords: "duplicates ceiling budget bytes hash",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::Rescan,
        group: Group::Scanning,
        label: "Rescan with these options",
        description: "Scan options apply to the next scan. Nothing rescans on its own or changes your place while editing.",
        keywords: "apply refresh next scan restart",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::File,
        group: Group::Advanced,
        label: "Analysis thresholds",
        description: "Analysis thresholds and other tuning options live in the settings file, which is JSON and meant to be edited.",
        keywords: "advanced configuration config preferences json settings file tuning threshold analysis rules cleanup age size",
        availability: Availability::Always,
    },
    OptionSpec {
        id: OptionId::Version,
        group: Group::Advanced,
        label: "Version",
        description: "The build identity to include in a bug report.",
        keywords: "about build version commit release bug report",
        availability: Availability::Always,
    },
];

#[derive(Default)]
struct SettingsFilter {
    terms: Vec<String>,
}
impl SettingsFilter {
    fn new(text: &str) -> Self {
        Self {
            terms: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }
    fn active(&self) -> bool {
        !self.terms.is_empty()
    }
    fn matches(&self, group: Group, fields: &[&str]) -> bool {
        if !self.active() {
            return true;
        }
        let haystack = format!("{} {}", group.label(), fields.join(" ")).to_lowercase();
        self.terms.iter().all(|term| haystack.contains(term))
    }
    fn option(&self, spec: &OptionSpec, location: &str) -> bool {
        self.matches(
            spec.group,
            &[
                spec.label,
                spec.description,
                spec.keywords,
                extra_description(spec.id),
                if spec.id == OptionId::File {
                    location
                } else {
                    ""
                },
            ],
        )
    }
}

fn extra_description(id: OptionId) -> &'static str {
    match id {
        OptionId::Owners if kosha_core::owner_costs_extra() => {
            "On Windows the ordinary scan asks the filesystem once per file. Reading the Master File Table makes owner collection free."
        }
        OptionId::Owners => {
            "Free here: the owner arrives with the same call that measures the file."
        }
        OptionId::Image if kosha_core::exact_size_costs_extra() => {
            "For WSL, reading the distribution's image avoids the Windows redirector and leaves out Windows-mounted files. It contains what Linux has written back, so the newest changes may be missing. Hold Shift when choosing a location to try its index or image for one scan."
        }
        _ => "",
    }
}

pub fn render(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> impl IntoElement {
    let input = app.settings_search();
    input.update(cx, |input, _| {
        input.set_editor_style(InputEditorStyle {
            foreground: palette.text,
            muted_foreground: palette.text_dim,
            background: palette.canvas,
            border: palette.text_faint,
            caret: palette.text,
            selection: palette.accent.opacity(0.25),
            ..InputEditorStyle::default()
        })
    });
    let text = input.read(cx).value().to_string();
    let filter = SettingsFilter::new(&text);
    let focus = input.read(cx).focus_handle(cx);
    let location = crate::settings::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "nowhere writable on this system".into());
    let mut sections = Vec::new();
    let mut count = 0;
    for group in Group::ALL {
        let mut rows = Vec::new();
        for spec in OPTIONS
            .iter()
            .filter(|spec| spec.group == group && spec.availability.available())
        {
            if filter.option(spec, &location) {
                rows.push(render_option(spec, app, palette, &location, cx));
            }
        }
        match group {
            Group::Saved => rows.extend(saved_searches(app, palette, &filter, cx)),
            Group::Pinned => rows.extend(pinned_folders(app, palette, &filter, cx)),
            _ => {}
        }
        if !rows.is_empty() {
            count += rows.len();
            sections.push(
                v_flex()
                    .flex_none()
                    .gap(px(5.0))
                    .pb(px(10.0))
                    .child(ui::section_label(palette, group.label()))
                    .children(rows)
                    .into_any_element(),
            );
        }
    }
    if sections.is_empty() {
        sections.push(v_flex().flex_none().gap(px(6.0)).py(px(20.0))
            .child(ui::note(palette, "No settings match your search."))
            .child(ui::hint(palette, "Try a setting name, an explanation, or a word such as theme, owners or thresholds."))
            .into_any_element());
    }
    let count = if filter.active() {
        format!(
            "{} {}",
            count,
            if count == 1 { "result" } else { "results" }
        )
    } else {
        "All settings".into()
    };

    ui::scrim("settings-scrim", palette)
        .items_start()
        .justify_end()
        .pt(metrics::scaled(metrics::TITLE_BAR_HEIGHT + 8.0))
        .pr(px(metrics::CARD_PADDING))
        .pb(px(metrics::CARD_PADDING))
        .on_click(cx.listener(|this, _, window, cx| this.close_settings(window, cx)))
        .child(
            ui::dialog("settings-panel", palette)
                .w(metrics::scaled(380.0))
                .max_w(relative(1.0))
                .gap(px(8.0))
                .max_h(relative(1.0))
                .child(
                    h_flex()
                        .flex_none()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_size(metrics::scaled(metrics::TEXT_LG))
                                .text_color(palette.text)
                                .child("Settings"),
                        )
                        .child(
                            ui::close("settings-close", palette, "Close (Esc)")
                                .debug_selector(|| "settings-close".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_settings(window, cx)
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .id("settings-search-field")
                        .flex_none()
                        .h(metrics::scaled(36.0))
                        .px(px(9.0))
                        .items_center()
                        .gap(px(6.0))
                        .rounded(px(6.0))
                        .bg(palette.canvas)
                        .border_1()
                        .border_color(palette.text_faint)
                        .track_focus(&focus)
                        .focus(|this| this.border_color(palette.accent))
                        .text_color(palette.text)
                        .text_size(metrics::scaled(metrics::TEXT_SM))
                        .child(div().flex_1().min_w(px(0.0)).child(Input::new(input)))
                        .when(!text.is_empty(), |field| {
                            field.child(
                                ui::glyph(
                                    "settings-search-clear",
                                    palette,
                                    "Clear settings search",
                                )
                                .debug_selector(|| "settings-search-clear".into())
                                .role(gpui_kit::Role::Button)
                                .aria_label("Clear settings search")
                                .child("\u{00d7}")
                                .hover(|this| this.bg(palette.hover).text_color(palette.text))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.clear_settings_search(window, cx),
                                )),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics::scaled(metrics::TEXT_XS))
                        .text_color(palette.text_faint)
                        .child(SharedString::from(count)),
                )
                .child(
                    v_flex()
                        .relative()
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_hidden()
                        .child(
                            v_flex()
                                .id("settings-body")
                                .flex_1()
                                .min_h(px(0.0))
                                .overflow_y_scroll()
                                .track_scroll(app.settings_scroll())
                                .pr(px(12.0))
                                .child(v_flex().flex_none().gap(px(4.0)).children(sections)),
                        )
                        .child(
                            div().absolute().inset_0().child(
                                Scrollbar::vertical(app.settings_scroll())
                                    .id("settings-scrollbar")
                                    .viewport_from_layout()
                                    .mode(ScrollbarMode::Always),
                            ),
                        ),
                ),
        )
        .into_any_element()
}

fn render_option(
    spec: &OptionSpec,
    app: &KoshaApp,
    palette: &Palette,
    location: &str,
    cx: &mut Context<KoshaApp>,
) -> AnyElement {
    let settings = app.settings();
    let control = match spec.id {
        OptionId::Updates => updates(app, palette, cx).into_any_element(),
        OptionId::Units => choice(
            palette,
            spec.label,
            &[
                ("GiB", settings.units == UnitPreference::Binary),
                ("GB", settings.units == UnitPreference::Decimal),
            ],
            cx,
            |index, settings| {
                settings.units = if index == 0 {
                    UnitPreference::Binary
                } else {
                    UnitPreference::Decimal
                }
            },
        )
        .into_any_element(),
        OptionId::TextSize => text_size(settings, palette, cx).into_any_element(),
        OptionId::Theme => choice(
            palette,
            spec.label,
            &[
                ("System", settings.theme == ThemePreference::System),
                ("Light", settings.theme == ThemePreference::Light),
                ("Dark", settings.theme == ThemePreference::Dark),
            ],
            cx,
            |index, settings| {
                settings.theme = match index {
                    0 => ThemePreference::System,
                    1 => ThemePreference::Light,
                    _ => ThemePreference::Dark,
                }
            },
        )
        .into_any_element(),
        OptionId::Accent => choice(
            palette,
            spec.label,
            &[
                ("System", settings.accent == AccentSource::System),
                ("kosha", settings.accent == AccentSource::Kosha),
            ],
            cx,
            |index, settings| {
                settings.accent = if index == 0 {
                    AccentSource::System
                } else {
                    AccentSource::Kosha
                }
            },
        )
        .into_any_element(),
        OptionId::Tint => toggle(palette, spec.label, settings.tint_chrome, cx, |settings| {
            settings.tint_chrome = !settings.tint_chrome
        })
        .into_any_element(),
        OptionId::Cross => toggle(
            palette,
            spec.label,
            settings.cross_filesystems,
            cx,
            |settings| settings.cross_filesystems = !settings.cross_filesystems,
        )
        .into_any_element(),
        OptionId::Exact => toggle(
            palette,
            spec.label,
            settings.exact_allocated,
            cx,
            |settings| settings.exact_allocated = !settings.exact_allocated,
        )
        .into_any_element(),
        OptionId::Hardlinks => toggle(
            palette,
            spec.label,
            settings.dedup_hardlinks,
            cx,
            |settings| settings.dedup_hardlinks = !settings.dedup_hardlinks,
        )
        .into_any_element(),
        OptionId::Mft => toggle(palette, spec.label, settings.read_mft, cx, |settings| {
            settings.read_mft = !settings.read_mft
        })
        .into_any_element(),
        OptionId::Image => toggle(palette, spec.label, settings.read_image, cx, |settings| {
            settings.read_image = !settings.read_image
        })
        .into_any_element(),
        OptionId::NativeWsl => toggle(
            palette,
            spec.label,
            settings.read_wsl_native,
            cx,
            |settings| settings.read_wsl_native = !settings.read_wsl_native,
        )
        .into_any_element(),
        OptionId::Owners => toggle(
            palette,
            spec.label,
            settings.collect_owners,
            cx,
            |settings| settings.collect_owners = !settings.collect_owners,
        )
        .into_any_element(),
        OptionId::SshAccess => toggle(palette, spec.label, settings.ssh_access, cx, |settings| {
            settings.ssh_access = !settings.ssh_access
        })
        .into_any_element(),
        OptionId::DuplicateMin => choice(
            palette,
            spec.label,
            &[
                ("Any size", settings.duplicate_min_mib == 0),
                ("1 MiB", settings.duplicate_min_mib == 1),
                ("16 MiB", settings.duplicate_min_mib == 16),
            ],
            cx,
            |index, settings| settings.duplicate_min_mib = [0, 1, 16][index],
        )
        .into_any_element(),
        OptionId::DuplicateBudget => choice(
            palette,
            spec.label,
            &[
                ("1 GiB", settings.duplicate_budget_gib == 1),
                ("8 GiB", settings.duplicate_budget_gib == 8),
                ("32 GiB", settings.duplicate_budget_gib == 32),
            ],
            cx,
            |index, settings| settings.duplicate_budget_gib = [1, 8, 32][index],
        )
        .into_any_element(),
        OptionId::Rescan => ui::primary("rescan", palette, false)
            .child(spec.label)
            .on_click(cx.listener(|this, _, window, cx| {
                this.close_settings(window, cx);
                this.rescan(cx);
            }))
            .into_any_element(),
        OptionId::File | OptionId::Version => div()
            .text_size(metrics::scaled(metrics::TEXT_SM))
            .text_color(palette.text)
            .child(spec.label)
            .into_any_element(),
    };
    let label = spec.label;
    let mut row = v_flex()
        .id(label)
        .debug_selector(move || format!("settings-option-{label}").into())
        .flex_none()
        .gap(px(4.0))
        .pb(px(5.0))
        .child(control)
        .child(ui::hint(palette, spec.description));
    let extra = extra_description(spec.id);
    if !extra.is_empty() {
        row = row.child(ui::hint(palette, extra));
    }
    if spec.id == OptionId::File {
        row = row.child(
            div()
                .min_w(px(0.0))
                .whitespace_normal()
                .text_size(metrics::scaled(metrics::TEXT_XS))
                .text_color(palette.text_dim)
                .child(SharedString::from(location.to_owned())),
        );
    }
    if spec.id == OptionId::Version {
        row = row.child(
            div()
                .text_size(metrics::scaled(metrics::TEXT_XS))
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(palette.text_faint)
                .child(SharedString::from(kosha_core::version::long())),
        );
    }
    row.into_any_element()
}

fn updates(app: &KoshaApp, palette: &Palette, cx: &mut Context<KoshaApp>) -> impl IntoElement {
    let status = if let Some(verification) = &app.update_verification {
        verification.clone()
    } else if app.update_busy {
        "Checking for updates…".into()
    } else if app.update_preference_error {
        "Could not save the update preference. Try again shortly.".into()
    } else {
        app.update_status.as_ref().map_or_else(
            || "Check GitHub for a newer stable release.".into(),
            kosha_update::Check::message,
        )
    };
    let downloads = match &app.update_status {
        Some(kosha_update::Check::Latest(release)) => release.release_url(),
        _ => kosha_update::RELEASES.to_owned(),
    };
    let packages = match &app.update_status {
        Some(kosha_update::Check::Latest(release)) if release.is_newer() => Some(release),
        _ => None,
    };
    #[cfg(target_os = "linux")]
    let linux_install = packages.is_some_and(|_| {
        kosha_update::Package::for_host().contains(&kosha_update::Package::LinuxArchive)
    });
    #[cfg(windows)]
    let windows_install = packages.is_some_and(|_| kosha_update::windows_setup_scope().is_some());
    #[cfg(target_os = "macos")]
    let macos_install = packages
        .is_some_and(|_| kosha_update::Package::for_host().contains(&kosha_update::Package::Dmg));
    let managed = matches!(app.update_status, Some(kosha_update::Check::Managed));
    let automatic = if app.update_automatic == Some(true) {
        "Turn off daily checks"
    } else {
        "Turn on daily checks"
    };
    let panel = v_flex()
        .gap(px(6.0))
        .child("Software updates")
        .child(ui::hint(
            palette,
            format!("Installed Kosha {} · {status}", kosha_core::version::NUMBER),
        ))
        .when_some(packages, |panel, release| {
            panel.child(
                h_flex().gap(px(6.0)).flex_wrap().children(
                    kosha_update::Package::for_host()
                        .iter()
                        .enumerate()
                        .map(|(index, &package)| {
                            let url = release.download_url(package);
                            ui::primary(("update-package", index), palette, false)
                                .focusable()
                                .tab_stop(true)
                                .child(package.label())
                                .on_click(move |_, _, cx| cx.open_url(&url))
                        })
                        .collect::<Vec<_>>(),
                ),
            )
        })
        .child(
            h_flex()
                .gap(px(6.0))
                .flex_wrap()
                .when(!managed, |buttons| {
                    buttons.child(
                        ui::button("update-check", palette)
                            .focusable()
                            .tab_stop(true)
                            .child("Check for updates")
                            .when(app.update_busy, |button| button.opacity(0.5))
                            .on_click(cx.listener(|app, _, _, cx| app.check_updates(true, cx))),
                    )
                })
                .when(managed, |buttons| {
                    buttons.child(
                        ui::button("update-store", palette)
                            .focusable()
                            .tab_stop(true)
                            .child("Open Microsoft Store updates")
                            .on_click(|_, _, cx| cx.open_url(kosha_update::STORE_UPDATES)),
                    )
                })
                .child(
                    ui::button("update-downloads", palette)
                        .focusable()
                        .tab_stop(true)
                        .child("Release downloads")
                        .on_click(move |_, _, cx| cx.open_url(&downloads)),
                )
                .child(
                    ui::button("update-guide", palette)
                        .focusable()
                        .tab_stop(true)
                        .child("Update guide")
                        .on_click(|_, _, cx| cx.open_url(kosha_update::GUIDE)),
                )
                .when(
                    matches!(app.update_status, Some(kosha_update::Check::Latest(_))),
                    |buttons| {
                        buttons.child(
                            ui::button("update-verify", palette)
                                .focusable()
                                .tab_stop(true)
                                .child("Verify downloaded file")
                                .when(app.update_busy, |button| button.opacity(0.5))
                                .on_click(
                                    cx.listener(|app, _, _, cx| app.verify_update_download(cx)),
                                ),
                        )
                    },
                ),
        )
        .when(!managed, |panel| {
            panel.child(
                ui::button("update-automatic", palette)
                    .focusable()
                    .tab_stop(true)
                    .child(automatic)
                    .when(app.update_busy, |button| button.opacity(0.5))
                    .on_click(cx.listener(|app, _, _, cx| app.toggle_update_checks(cx))),
            )
        });
    #[cfg(target_os = "linux")]
    let panel = panel.when(linux_install, |panel| {
        panel.child(ui::hint(palette, "Install a managed copy for your account using ~/.local/bin. Existing launchers are preserved."))
            .child(ui::primary("update-install-linux", palette, false)
                .focusable()
                .tab_stop(true)
                .child("Download and install update")
                .when(app.update_busy, |button| button.opacity(0.5))
                .on_click(cx.listener(|app, _, _, cx| app.install_release_update(cx))))
    });
    #[cfg(windows)]
    let panel = panel.when(windows_install, |panel| {
        panel
            .child(ui::hint(
                palette,
                "Open the verified installer, then close Kosha and finish the upgrade in Setup.",
            ))
            .child(
                ui::primary("update-install-windows", palette, false)
                    .focusable()
                    .tab_stop(true)
                    .child("Download and open Setup")
                    .when(app.update_busy, |button| button.opacity(0.5))
                    .on_click(cx.listener(|app, _, _, cx| app.install_release_update(cx))),
            )
    });
    #[cfg(target_os = "macos")]
    let panel = panel.when(macos_install, |panel| {
        panel
            .child(ui::hint(
                palette,
                "Open the verified disk image, then replace Kosha in Applications.",
            ))
            .child(
                ui::primary("update-install-macos", palette, false)
                    .focusable()
                    .tab_stop(true)
                    .child("Download and open DMG")
                    .when(app.update_busy, |button| button.opacity(0.5))
                    .on_click(cx.listener(|app, _, _, cx| app.install_release_update(cx))),
            )
    });
    panel
}

fn text_size(
    settings: &Settings,
    palette: &Palette,
    cx: &mut Context<KoshaApp>,
) -> impl IntoElement {
    let percent = settings.text_scale_percent();
    let step = |label, hint, delta, disabled: bool, cx: &mut Context<KoshaApp>| {
        ui::button(hint, palette)
            .when(!disabled, |button| button.focusable().tab_stop(true))
            .focus(|button| button.border_color(palette.accent))
            .child(label)
            .when(disabled, |button| button.opacity(0.5).cursor_default())
            .tooltip(move |window, cx| ui::tooltip(hint).build(window, cx))
            .on_click(cx.listener(move |app, _, _, cx| app.step_text_scale(delta, cx)))
    };
    v_flex().gap(px(6.0)).child("Text size").child(
        h_flex()
            .gap(px(6.0))
            .flex_wrap()
            .child(step(
                "−",
                "Decrease text size",
                -10,
                percent == TEXT_SCALE_MIN,
                cx,
            ))
            .child(
                div()
                    .min_w(metrics::scaled(40.0))
                    .text_center()
                    .child(format!("{percent}%")),
            )
            .child(step(
                "+",
                "Increase text size",
                10,
                percent == TEXT_SCALE_MAX,
                cx,
            ))
            .child(
                ui::button("text-size-system", palette)
                    .focusable()
                    .tab_stop(true)
                    .focus(|button| button.border_color(palette.accent))
                    .child("System default")
                    .when(settings.text_scale.is_none(), |button| {
                        button
                            .border_color(palette.accent)
                            .text_color(palette.accent)
                    })
                    .on_click(cx.listener(|app, _, _, cx| app.reset_text_scale(cx))),
            ),
    )
}

/// The kept queries, each removable.
///
/// The palette can forget the search currently in the box, which is the fast
/// path and the wrong one for tidying up: it would mean running a query in
/// order to stop keeping it. This is where a list you no longer want goes away.
fn saved_searches(
    app: &KoshaApp,
    palette: &Palette,
    filter: &SettingsFilter,
    cx: &mut Context<KoshaApp>,
) -> Vec<AnyElement> {
    let settings = app.settings();
    if settings.saved_queries.is_empty() {
        if !filter.matches(
            Group::Saved,
            &[
                "Saved searches",
                "Keep a query with the star",
                "bookmark favorite favourite",
            ],
        ) {
            return Vec::new();
        }
        return vec![
            ui::hint(
                palette,
                "None yet. The star beside the search box keeps the query that is \
                 running, and ⌘/Ctrl+K finds it again.",
            )
            .into_any_element(),
        ];
    }

    // Built in a loop rather than `map`: `cx` is a unique borrow and cannot be
    // captured by the `FnMut` an iterator adaptor would need.
    let mut rows = Vec::with_capacity(settings.saved_queries.len());
    for (index, saved) in settings.saved_queries.iter().enumerate() {
        if !filter.matches(
            Group::Saved,
            &[
                &saved.name,
                &saved.query,
                "saved bookmark favorite favourite query",
            ],
        ) {
            continue;
        }
        let query = saved.query.clone();
        let named = saved.name != saved.query;
        let forgetting = app.armed(&crate::app::Armed::Query(saved.query.clone()));

        rows.push(
            h_flex()
                .id(("saved", index))
                .flex_none()
                .py(px(4.0))
                .px(px(6.0))
                .gap(px(8.0))
                .items_center()
                .rounded(px(metrics::RADIUS_SM))
                .hover(|this| this.bg(palette.hover))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .child(
                            div()
                                .text_size(metrics::scaled(metrics::TEXT_SM))
                                .text_color(palette.text_dim)
                                .child(SharedString::from(saved.name.clone())),
                        )
                        // Only when the name is not already the query, which is
                        // the case until somebody edits the file.
                        .when(named, |this| {
                            this.child(
                                div()
                                    .text_size(metrics::scaled(metrics::TEXT_XS))
                                    .text_color(palette.text_faint)
                                    .child(SharedString::from(saved.query.clone())),
                            )
                        }),
                )
                .child(
                    ui::remove(
                        ("forget", index),
                        palette,
                        "Forget this search. Nothing on disk is touched.",
                        "Forget",
                        forgetting,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.forget_saved_query(&query, window, cx);
                    })),
                )
                .into_any_element(),
        );
    }
    rows
}

/// The pinned folders, each removable.
fn pinned_folders(
    app: &KoshaApp,
    palette: &Palette,
    filter: &SettingsFilter,
    cx: &mut Context<KoshaApp>,
) -> Vec<AnyElement> {
    let settings = app.settings();
    if settings.saved_scopes.is_empty() {
        if !filter.matches(
            Group::Pinned,
            &[
                "Pinned folders",
                "Keep a folder in the sidebar",
                "bookmark location",
            ],
        ) {
            return Vec::new();
        }
        return vec![
            ui::hint(
                palette,
                "None yet. ⌘/Ctrl+K, then \"Pin\", keeps the folder you are looking \
                 at one click away in the sidebar.",
            )
            .into_any_element(),
        ];
    }

    let mut rows = Vec::with_capacity(settings.saved_scopes.len());
    for (index, scope) in settings.saved_scopes.iter().enumerate() {
        let shown_path = scope.path.display().to_string();
        if !filter.matches(
            Group::Pinned,
            &[&scope.name, &shown_path, "pinned bookmark sidebar location"],
        ) {
            continue;
        }
        let path = scope.path.clone();
        let unpinning = app.armed(&crate::app::Armed::Pin(scope.path.clone()));
        rows.push(
            h_flex()
                .id(("scope-setting", index))
                .flex_none()
                .py(px(4.0))
                .px(px(6.0))
                .gap(px(8.0))
                .items_center()
                .rounded(px(metrics::RADIUS_SM))
                .hover(|this| this.bg(palette.hover))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .child(
                            div()
                                .text_size(metrics::scaled(metrics::TEXT_SM))
                                .text_color(palette.text_dim)
                                .child(SharedString::from(scope.name.clone())),
                        )
                        .child(
                            div()
                                .text_size(metrics::scaled(metrics::TEXT_XS))
                                .text_color(palette.text_faint)
                                .child(SharedString::from(scope.path.display().to_string())),
                        ),
                )
                .child(
                    ui::remove(
                        ("unpin", index),
                        palette,
                        "Take this folder off the sidebar. Nothing on disk is touched.",
                        "Unpin",
                        unpinning,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.unpin_from_settings(&path, window, cx);
                    })),
                )
                .into_any_element(),
        );
    }
    rows
}

/// A labelled on/off row. The whole row is the target, not just the pill.
fn toggle(
    palette: &Palette,
    label: &'static str,
    on: bool,
    cx: &mut Context<KoshaApp>,
    edit: impl Fn(&mut Settings) + 'static,
) -> impl IntoElement {
    h_flex()
        .id(label)
        .cursor_pointer()
        .h(metrics::scaled(metrics::ROW_HEIGHT))
        .justify_between()
        .items_center()
        .rounded(px(metrics::RADIUS_SM))
        .text_size(metrics::scaled(metrics::TEXT))
        .text_color(palette.text)
        .hover(|this| this.bg(palette.hover))
        .child(div().child(label))
        .child(
            div()
                .w(px(34.0))
                .h(px(18.0))
                .p(px(2.0))
                .rounded(px(9.0))
                .when(on, |this| this.bg(palette.accent).flex().justify_end())
                .when(!on, |this| this.bg(palette.hairline))
                .child(div().w(px(14.0)).h(px(14.0)).rounded(px(7.0)).bg(if on {
                    palette.accent_text
                } else {
                    palette.text_faint
                })),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.update_settings(&edit, window, cx);
        }))
}

/// A labelled row of mutually exclusive options.
fn choice(
    palette: &Palette,
    label: &'static str,
    options: &[(&'static str, bool)],
    cx: &mut Context<KoshaApp>,
    edit: impl Fn(usize, &mut Settings) + Clone + 'static,
) -> impl IntoElement {
    let mut buttons = Vec::with_capacity(options.len());
    for (index, (text, selected)) in options.iter().enumerate() {
        let (text, selected) = (*text, *selected);
        let edit = edit.clone();
        buttons.push(
            // Labels repeat between rows (notably Theme/System and
            // Accent/System). GPUI routes state and clicks by element ID;
            // sharing that ID can send an accent click to the theme row.
            ui::chip((label, index))
                .when(selected, |this| {
                    this.bg(palette.accent).text_color(palette.accent_text)
                })
                .when(!selected, |this| {
                    this.text_color(palette.text_dim)
                        .hover(|this| this.bg(palette.hover))
                })
                .child(text)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.update_settings(|settings| edit(index, settings), window, cx);
                }))
                .into_any_element(),
        );
    }

    h_flex()
        .h(metrics::scaled(metrics::ROW_HEIGHT))
        .justify_between()
        .items_center()
        .text_size(metrics::scaled(metrics::TEXT))
        .text_color(palette.text)
        .child(div().child(label))
        .child(
            h_flex()
                .gap(px(2.0))
                .p(px(2.0))
                .rounded(px(6.0))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.hairline)
                .children(buttons),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(text: &str) -> Vec<OptionId> {
        let filter = SettingsFilter::new(text);
        OPTIONS
            .iter()
            .filter(|spec| filter.option(spec, "J:/config/kosha/settings.json"))
            .map(|spec| spec.id)
            .collect()
    }

    #[test]
    fn settings_are_findable_by_visible_words_and_common_names() {
        assert_eq!(matches("font dpi"), vec![OptionId::TextSize]);
        assert!(matches("Theme").contains(&OptionId::Theme));
        assert_eq!(matches("slow compressed"), vec![OptionId::Exact]);
        assert_eq!(matches("mft admin"), vec![OptionId::Mft]);
        assert_eq!(matches("configuration thresholds"), vec![OptionId::File]);
        assert_eq!(matches("settings.json"), vec![OptionId::File]);
        assert_eq!(matches("owner next"), vec![OptionId::Owners]);
        assert_eq!(matches("owners"), vec![OptionId::Owners]);
        assert_eq!(matches("ssh remote"), vec![OptionId::SshAccess]);
    }

    #[test]
    fn group_search_is_case_insensitive_and_words_narrow_the_same_row() {
        assert_eq!(
            matches(" DISPLAY "),
            vec![
                OptionId::TextSize,
                OptionId::Units,
                OptionId::Theme,
                OptionId::Accent,
                OptionId::Tint
            ]
        );
        assert_eq!(
            matches("display desktop"),
            vec![OptionId::TextSize, OptionId::Accent]
        );
        assert_eq!(matches("\tTHEME  DARK\n"), vec![OptionId::Theme]);
        assert!(matches("theme owner").is_empty());
        assert!(matches("no-such-setting-9c8e").is_empty());
        assert_eq!(matches(" \t\n").len(), OPTIONS.len());
    }

    #[test]
    fn saved_searches_and_pinned_paths_use_the_same_filter() {
        let query = SettingsFilter::new("saved markdown");
        assert!(query.matches(Group::Saved, &["Markdown files", "file and ext = md"]));
        assert!(!query.matches(Group::Saved, &["Large files", "size > 1gib"]));
        let path = SettingsFilter::new("pinned rust");
        assert!(path.matches(Group::Pinned, &["Projects", "J:/work/Rust"]));
        assert!(!path.matches(Group::Pinned, &["Pictures", "J:/photos"]));
        assert!(SettingsFilter::new("résumé").matches(Group::Pinned, &["Résumé", "J:/documents"]));
    }
}
