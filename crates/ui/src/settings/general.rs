//! Settings → General: local window-launch behavior and desktop updates.

use gpui::{AnyElement, Context, EventEmitter, IntoElement, Render, Window, div, prelude::*, px};

use crate::settings::{WindowMode, widgets};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum GeneralEvent {
    WindowModeChanged(WindowMode),
    AutomaticUpdatesChanged(bool),
}

pub struct GeneralPage {
    window_mode: WindowMode,
    automatic_updates: bool,
}

impl EventEmitter<GeneralEvent> for GeneralPage {}

impl GeneralPage {
    pub fn new(window_mode: WindowMode, automatic_updates: bool, _cx: &mut Context<Self>) -> Self {
        Self {
            window_mode,
            automatic_updates,
        }
    }
}

fn window_mode_detail(mode: WindowMode) -> &'static str {
    match mode {
        WindowMode::RememberLastSize => "Continue where you left off",
        WindowMode::FitScreen => "Fit inside the current display",
        WindowMode::Maximized => "Use all available space",
    }
}

/// A real miniature window, scaled differently for each launch mode. The old
/// picker supplied an empty `div`, which made all three choices blank.
fn window_preview(theme: &Theme, mode: WindowMode) -> AnyElement {
    let (width, height) = match mode {
        WindowMode::RememberLastSize => (0.62, 42.0),
        WindowMode::FitScreen => (0.80, 52.0),
        WindowMode::Maximized => (0.94, 58.0),
    };
    let line = theme.text.opacity(0.18);

    div()
        .h(px(66.0))
        .w_full()
        .rounded(px(8.0))
        .bg(theme.bg)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(gpui::relative(width))
                .h(px(height))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(9.0))
                        .flex_none()
                        .border_b_1()
                        .border_color(theme.border)
                        .px(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .children((0..3).map(|_| {
                            div()
                                .size(px(2.5))
                                .rounded_full()
                                .bg(theme.text_muted.opacity(0.35))
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .child(
                            div()
                                .w(gpui::relative(0.24))
                                .h_full()
                                .border_r_1()
                                .border_color(theme.border)
                                .bg(theme.surface_raised)
                                .flex()
                                .flex_col()
                                .gap(px(3.0))
                                .p(px(4.0))
                                .child(div().h(px(2.5)).w_full().rounded_full().bg(line))
                                .child(
                                    div()
                                        .h(px(2.5))
                                        .w(gpui::relative(0.72))
                                        .rounded_full()
                                        .bg(line),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .p(px(6.0))
                                .child(
                                    div()
                                        .h(px(3.0))
                                        .w(gpui::relative(0.68))
                                        .rounded_full()
                                        .bg(theme.text.opacity(0.26)),
                                )
                                .child(div().h(px(2.5)).w_full().rounded_full().bg(line))
                                .child(
                                    div()
                                        .h(px(2.5))
                                        .w(gpui::relative(0.82))
                                        .rounded_full()
                                        .bg(line),
                                ),
                        ),
                ),
        )
        .into_any_element()
}

fn window_option(
    theme: &Theme,
    mode: WindowMode,
    selected: bool,
    preview: AnyElement,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .p(px(8.0))
        .rounded(px(12.0))
        .border_2()
        .border_color(if selected { theme.accent } else { theme.border })
        .bg(if selected {
            theme.accent.opacity(0.045)
        } else {
            theme.card_glass_bg()
        })
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_raised_hover))
        .child(preview)
        .child(
            div()
                .mt(px(9.0))
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .size(px(14.0))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(if selected { theme.accent } else { theme.border })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |el| {
                            el.child(div().size(px(6.0)).rounded_full().bg(theme.accent))
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(mode.label()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(10.0))
                                .text_color(theme.text_muted)
                                .child(window_mode_detail(mode)),
                        ),
                ),
        )
}

impl Render for GeneralPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let window_mode = self.window_mode;
        let automatic_updates = self.automatic_updates;

        div()
            .id("general-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(&theme, "General", None))
                    .child(widgets::page_subtitle(
                        &theme,
                        "Choose how Zeron opens and keeps itself up to date on this device.",
                    ))
                    .child(
                        div()
                            .mt(px(32.0))
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
                            .child(widgets::field_label(&theme, "Window on launch"))
                            .child(
                                div().flex().gap(px(12.0)).children(
                                    WindowMode::ALL.into_iter().map(|mode| {
                                        window_option(
                                            &theme,
                                            mode,
                                            mode == window_mode,
                                            window_preview(&theme, mode),
                                        )
                                        .id(gpui::SharedString::from(format!(
                                            "window-mode-{}",
                                            mode.label()
                                        )))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.window_mode = mode;
                                            cx.emit(GeneralEvent::WindowModeChanged(mode));
                                            cx.notify();
                                        }))
                                    }),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(32.0))
                            .child(widgets::field_label(&theme, "Software updates"))
                            .child(widgets::section_card(&theme).child(
                                widgets::card_row(&theme, true)
                                    .child(widgets::row_tile(&theme, crate::icons::REFRESH))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .child(widgets::row_title(
                                                &theme,
                                                "Automatic updates",
                                            ))
                                            .child(
                                                div()
                                                    .mt(px(3.0))
                                                    .text_size(px(12.0))
                                                    .text_color(theme.text_muted)
                                                    .child(
                                                        "Download verified updates in the background and ask before restarting.",
                                                    ),
                                            ),
                                    )
                                    .child(
                                        widgets::toggle_switch(&theme, automatic_updates)
                                            .id("automatic-updates-toggle")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.automatic_updates = !this.automatic_updates;
                                                cx.emit(GeneralEvent::AutomaticUpdatesChanged(
                                                    this.automatic_updates,
                                                ));
                                                cx.notify();
                                            })),
                                    ),
                            )),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_window_mode_has_distinct_copy() {
        let details = WindowMode::ALL.map(window_mode_detail);
        assert!(details.iter().all(|detail| !detail.is_empty()));
        assert_ne!(details[0], details[1]);
        assert_ne!(details[1], details[2]);
    }
}
