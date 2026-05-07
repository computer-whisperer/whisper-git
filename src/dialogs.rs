//! Modal dialog compositions.
//!
//! Each modal is an aetna `overlay([scrim, modal_panel(...)])`. The
//! scrim emits `{key}:dismiss` on outside-click; Escape is handled
//! globally by aetna and arrives as `UiEventKind::Escape` so the host
//! can close whichever modal is currently active.

use aetna_core::{El, IconName, prelude::*};

use crate::config::Config;

pub const MODAL_SETTINGS_KEY: &str = "modal:settings";
pub const MODAL_CONFIRM_KEY: &str = "modal:confirm";
pub const MODAL_ERROR_KEY: &str = "modal:error";

/// Settings panel: small subset of `Config` is editable for now —
/// avatars, shortcut bar visibility, row scale. Other options
/// (orphans, ratchet scroll, time spacing) get added as their callers
/// come online.
pub fn settings_modal(config: &Config, shortcut_bar_visible: bool) -> El {
    let body = column([
        field_row(
            "Show avatars",
            switch(config.avatars_enabled).key("settings:avatars"),
        ),
        field_row(
            "Show shortcut bar",
            switch(shortcut_bar_visible).key("settings:shortcut_bar"),
        ),
        field_row("Row size", row_size_selector(config.row_scale)),
        row([spacer(), button("Done").key("settings:close").primary()]).align(Align::Center),
    ])
    .gap(tokens::SPACE_MD);

    overlays_panel(MODAL_SETTINGS_KEY, "Settings", [body])
}

fn row_size_selector(current: f32) -> El {
    let normal_active = current < 1.25;
    let normal = button("Normal").key("settings:row_size:1.0");
    let normal = if normal_active {
        normal.primary()
    } else {
        normal.ghost()
    };
    let large = button("Large").key("settings:row_size:1.5");
    let large = if normal_active {
        large.ghost()
    } else {
        large.primary()
    };
    row([normal, large]).gap(tokens::SPACE_XS)
}

/// Generic confirm dialog. `ok_label` is typically "Delete" /
/// "Discard" / "Reset"; `destructive` switches the OK button's role.
pub fn confirm_modal(title: &str, body: &str, ok_label: &str, destructive: bool) -> El {
    let body_el = column([
        paragraph(body.to_string()),
        row([
            spacer(),
            button("Cancel").key("modal:confirm:cancel").ghost(),
            if destructive {
                button(ok_label.to_string())
                    .key("modal:confirm:ok")
                    .destructive()
            } else {
                button(ok_label.to_string())
                    .key("modal:confirm:ok")
                    .primary()
            },
        ])
        .gap(tokens::SPACE_SM)
        .align(Align::Center),
    ])
    .gap(tokens::SPACE_MD);

    overlays_panel(MODAL_CONFIRM_KEY, title, [body_el])
}

pub fn error_modal(title: &str, body: &str) -> El {
    let body_el = column([
        row([
            icon(IconName::AlertCircle).text_color(tokens::DESTRUCTIVE),
            paragraph(body.to_string()),
        ])
        .gap(tokens::SPACE_SM)
        .align(Align::Center),
        row([
            spacer(),
            button("Dismiss").key("modal:error:close").primary(),
        ])
        .align(Align::Center),
    ])
    .gap(tokens::SPACE_MD);

    overlays_panel(MODAL_ERROR_KEY, title, [body_el])
}

fn overlays_panel<I, E>(key: &str, title: &str, body: I) -> El
where
    I: IntoIterator<Item = E>,
    E: Into<El>,
{
    overlay([scrim(format!("{key}:dismiss")), modal_panel(title, body)])
}
