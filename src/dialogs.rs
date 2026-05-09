//! Modal dialog compositions.
//!
//! Each modal is an aetna `overlay([scrim, modal_panel(...)])`. The
//! scrim emits `{key}:dismiss` on outside-click; Escape is handled
//! globally by aetna and arrives as `UiEventKind::Escape` so the host
//! can close whichever modal is currently active.

use aetna_core::{El, IconName, Selection, prelude::*};

use crate::config::Config;

pub const MODAL_SETTINGS_KEY: &str = "modal:settings";
pub const MODAL_CONFIRM_KEY: &str = "modal:confirm";
pub const MODAL_ERROR_KEY: &str = "modal:error";
pub const MODAL_CLONE_KEY: &str = "modal:clone";
pub const MODAL_TOKEN_KEY: &str = "modal:token";

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
        row([
            button("Clone repository\u{2026}")
                .key("settings:clone")
                .ghost(),
            button("Manage tokens\u{2026}")
                .key("settings:tokens")
                .ghost(),
            spacer(),
            button("Done").key("settings:close").primary(),
        ])
        .gap(tokens::SPACE_2)
        .align(Align::Center),
    ])
    .gap(tokens::SPACE_3);

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
    row([normal, large]).gap(tokens::SPACE_1)
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
        .gap(tokens::SPACE_2)
        .align(Align::Center),
    ])
    .gap(tokens::SPACE_3);

    overlays_panel(MODAL_CONFIRM_KEY, title, [body_el])
}

pub fn error_modal(title: &str, body: &str) -> El {
    let body_el = column([
        row([
            icon(IconName::AlertCircle).text_color(tokens::DESTRUCTIVE),
            paragraph(body.to_string()),
        ])
        .gap(tokens::SPACE_2)
        .align(Align::Center),
        row([
            spacer(),
            button("Dismiss").key("modal:error:close").primary(),
        ])
        .align(Align::Center),
    ])
    .gap(tokens::SPACE_3);

    overlays_panel(MODAL_ERROR_KEY, title, [body_el])
}

fn overlays_panel<I, E>(key: &str, title: &str, body: I) -> El
where
    I: IntoIterator<Item = E>,
    E: Into<El>,
{
    overlay([scrim(format!("{key}:dismiss")), modal_panel(title, body)])
}

/// Form state for the Clone modal — controlled inputs plus the bare
/// flag. Owned by `WhisperApp` so the modal can stay open across
/// frames and the user's text edits persist.
#[derive(Clone, Debug, Default)]
pub struct CloneForm {
    pub url: String,
    pub dest: String,
    pub bare: bool,
}

/// Clone-a-remote modal. URL + destination + bare checkbox; `Browse…`
/// opens a native folder picker (handled by `ui_app`); `Clone` kicks
/// off `git::clone_async` and closes the modal. The async result lands
/// in `WhisperApp::clone_op` and either creates a new tab or surfaces
/// an Error modal.
pub fn clone_modal(state: &CloneForm, selection: &Selection, in_flight: bool) -> El {
    // Long fields stack their label above the control via `form_item` —
    // `field_row` would squeeze a URL or path onto the right edge of the
    // panel, which the README's catalog explicitly flags as the wrong
    // shape for stacked-field intent.
    let url_field = form_item([
        form_label("Repository URL"),
        form_control(
            text_input(&state.url, selection, "clone:url")
                .key("clone:url")
                .width(Size::Fill(1.0)),
        ),
    ]);

    let dest_field = form_item([
        form_label("Destination"),
        form_control(
            row([
                text_input(&state.dest, selection, "clone:dest")
                    .key("clone:dest")
                    .width(Size::Fill(1.0)),
                button("Browse\u{2026}").key("clone:browse").ghost(),
            ])
            .gap(tokens::SPACE_2)
            .align(Align::Center)
            .width(Size::Fill(1.0)),
        ),
    ]);

    let bare_field = field_row("Bare clone", switch(state.bare).key("clone:bare"));

    let primary = if in_flight {
        // Disabled-ish: the action handler short-circuits when an op is
        // already in flight, and the muted ghost style makes the
        // disabled state visually obvious.
        button("Cloning\u{2026}").key("clone:start").ghost()
    } else {
        button("Clone").key("clone:start").primary()
    };
    let actions = row([
        spacer(),
        button("Cancel").key("modal:clone:cancel").ghost(),
        primary,
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center);

    let body = form([url_field, dest_field, bare_field, actions]);

    overlays_panel(MODAL_CLONE_KEY, "Clone repository", [body])
}

/// Form state for the Token modal — currently just the GitHub token
/// input (GitLab multi-host support comes back when we re-enable
/// `gitlab.rs`).
#[derive(Clone, Debug, Default)]
pub struct TokenForm {
    /// Live text-input contents. Empty when the user hasn't started
    /// editing or just cleared the existing value.
    pub github_input: String,
    /// `true` while the user is actively editing the GitHub field —
    /// drives the Save / Cancel button pair vs the Set / Clear pair.
    pub editing_github: bool,
}

/// Token management modal. Mirrors the old `token_dialog` but trimmed
/// to GitHub — Set / Replace / Clear inline controls, no GitLab
/// multi-host list yet. The actual keychain reads/writes go through
/// `token_store`.
pub fn token_modal(state: &TokenForm, selection: &Selection, github_set: bool) -> El {
    let github_controls: El = if state.editing_github {
        row([
            text_input(&state.github_input, selection, "token:github")
                .key("token:github")
                .width(Size::Fill(1.0)),
            button("Save").key("token:github:save").primary(),
            button("Cancel").key("token:github:cancel").ghost(),
        ])
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .width(Size::Fill(1.0))
    } else {
        let status: El = if github_set {
            badge("Configured").success()
        } else {
            badge("Not set").muted()
        };
        let mut children: Vec<El> = vec![
            status,
            spacer(),
            button(if github_set {
                "Replace\u{2026}"
            } else {
                "Set\u{2026}"
            })
            .key("token:github:edit")
            .ghost(),
        ];
        if github_set {
            children.push(button("Clear").key("token:github:clear").destructive());
        }
        row(children)
            .gap(tokens::SPACE_2)
            .align(Align::Center)
            .width(Size::Fill(1.0))
    };

    let github_block = form_item([
        form_label("GitHub"),
        form_control(github_controls),
        form_description("Stored in the system keychain via the `keyring` crate."),
    ]);

    let actions = row([spacer(), button("Done").key("modal:token:close").primary()])
        .align(Align::Center);

    let body = form([github_block, actions]);

    overlays_panel(MODAL_TOKEN_KEY, "Manage tokens", [body])
}
