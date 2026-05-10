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
pub const MODAL_BRANCH_KEY: &str = "modal:branch";
pub const MODAL_TAG_KEY: &str = "modal:tag";
pub const MODAL_PULL_KEY: &str = "modal:pull";
pub const MODAL_OPEN_REPO_KEY: &str = "modal:open_repo";

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

/// Open-repository picker. Shown when the tab-bar `+` is clicked.
/// Two action buttons (Open Local… / Clone Remote…) plus a recent-repos
/// list keyed `modal:open_repo:recent:{idx}` so users with tabs open
/// can reach a recent path without a file dialog round-trip. Mirrors
/// the welcome view's affordances so the muscle memory is the same.
pub fn open_repo_modal(recent: &[String]) -> El {
    let actions = row([
        button_with_icon(IconName::Folder, "Open Local\u{2026}")
            .key("modal:open_repo:browse")
            .primary(),
        button_with_icon(IconName::Download, "Clone Remote\u{2026}")
            .key("modal:open_repo:clone"),
        spacer(),
        button("Cancel").key("modal:open_repo:cancel").ghost(),
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center);

    let mut body_children: Vec<El> = vec![actions];
    if !recent.is_empty() {
        let rows = recent
            .iter()
            .enumerate()
            .map(|(idx, path)| recent_repo_row(idx, path));
        body_children.push(
            column([
                row([h3("Recent"), spacer()])
                    .align(Align::Center)
                    .width(Size::Fill(1.0)),
                item_group(rows),
            ])
            .gap(tokens::SPACE_2)
            .width(Size::Fill(1.0)),
        );
    }

    let body = column(body_children).gap(tokens::SPACE_4);
    overlays_panel(MODAL_OPEN_REPO_KEY, "Open repository", [body])
}

fn recent_repo_row(idx: usize, path_str: &str) -> El {
    let path = std::path::Path::new(path_str);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.to_string());
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    item([
        item_media_icon(IconName::Folder),
        item_content([item_title(name), item_description(parent)]),
    ])
    .key(format!("modal:open_repo:recent:{idx}"))
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

/// Form state for the Create Branch modal — controlled name input
/// plus the "checkout after create" toggle. Owned by `WhisperApp`
/// so the user's typing persists across frames.
#[derive(Clone, Debug, Default)]
pub struct BranchForm {
    pub name: String,
    /// Default `true` — matches the typical "make and switch" flow
    /// users expect from a `git checkout -b` muscle memory.
    pub checkout: bool,
}

/// Create-a-branch modal. Name input + a "Check out after creating"
/// toggle + a small caption showing what commit the branch will be
/// created at (the focused tab's selected_commit when one is open,
/// otherwise the active worktree's HEAD). The action handler routes
/// through GitRepo::create_branch_at and optionally checkout_branch.
pub fn branch_modal(state: &BranchForm, selection: &Selection, target_short: &str) -> El {
    let name_field = form_item([
        form_label("Branch name"),
        form_control(
            text_input(&state.name, selection, "branch:name")
                .key("branch:name")
                .width(Size::Fill(1.0)),
        ),
        form_description(format!("Will be created at {target_short}.")),
    ]);

    let checkout_field = field_row(
        "Check out after creating",
        switch(state.checkout).key("branch:checkout"),
    );

    let actions = row([
        spacer(),
        button("Cancel").key("modal:branch:cancel").ghost(),
        button("Create").key("branch:create").primary(),
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center);

    let body = form([name_field, checkout_field, actions]);

    overlays_panel(MODAL_BRANCH_KEY, "Create branch", [body])
}

/// Form state for the Create Tag modal — controlled name input. Tags
/// are lightweight (`tag_lightweight`), so no message field; HEAD
/// doesn't move, so no "checkout after" toggle.
#[derive(Clone, Debug, Default)]
pub struct TagForm {
    pub name: String,
}

/// Create-a-tag modal. Name input + a small caption showing what
/// commit the tag will point at. Routes through GitRepo::create_tag.
pub fn tag_modal(state: &TagForm, selection: &Selection, target_short: &str) -> El {
    let name_field = form_item([
        form_label("Tag name"),
        form_control(
            text_input(&state.name, selection, "tag:name")
                .key("tag:name")
                .width(Size::Fill(1.0)),
        ),
        form_description(format!("Will be created at {target_short}.")),
    ]);

    let actions = row([
        spacer(),
        button("Cancel").key("modal:tag:cancel").ghost(),
        button("Create").key("tag:create").primary(),
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center);

    let body = form([name_field, actions]);

    overlays_panel(MODAL_TAG_KEY, "Create tag", [body])
}

/// Form state for the Pull picker modal — one selected source label
/// (e.g. "origin/main") plus the `--rebase` toggle. Populated on open
/// from the active branch's upstream when present, falling back to
/// `origin/<current_branch>`. Empty `source` keeps the Pull button
/// disabled — the modal can also surface for repos with no remote-
/// tracking branches at all (just shows an empty list).
#[derive(Clone, Debug, Default)]
pub struct PullForm {
    pub source: String,
    pub rebase: bool,
}

/// Pull-from-remote modal. Lists every remote-tracking branch as a
/// radio option, plus a `--rebase` switch. The plain Pull header
/// button keeps the default tracking-branch shortcut; this modal is
/// reached via the small caret next to it.
pub fn pull_modal(state: &PullForm, sources: &[String]) -> El {
    let radio = radio_group(
        "pull:source",
        &state.source,
        sources.iter().map(|s| (s.clone(), s.clone())),
    );
    let source_field = form_item([form_label("Source"), radio]);

    let rebase_field = field_row(
        "Rebase instead of merge",
        switch(state.rebase).key("pull:rebase"),
    );

    let mut pull_btn = button("Pull").key("pull:execute").primary();
    if state.source.is_empty() {
        pull_btn = pull_btn.disabled();
    }

    let actions = row([
        spacer(),
        button("Cancel").key("modal:pull:cancel").ghost(),
        pull_btn,
    ])
    .gap(tokens::SPACE_2)
    .align(Align::Center);

    let body = form([source_field, rebase_field, actions]);

    overlays_panel(MODAL_PULL_KEY, "Pull from remote", [body])
}

/// Form state for the Token modal — one section for GitHub, one row
/// per registered GitLab host. The Vec/HashMap shape mirrors the
/// modal's render path: each host is independently editable, and we
/// only carry input state for hosts the user is actively editing.
#[derive(Clone, Debug, Default)]
pub struct TokenForm {
    /// Live text-input contents for the GitHub field. Empty when the
    /// user hasn't started editing or just cleared the existing value.
    pub github_input: String,
    /// `true` while the user is actively editing the GitHub field —
    /// drives the Save / Cancel button pair vs the Set / Clear pair.
    pub editing_github: bool,
    /// Per-host GitLab input buffers. A host being a key here means the
    /// row is in editing mode; absence means the row shows status +
    /// Set/Replace/Clear buttons.
    pub gitlab_inputs: std::collections::HashMap<String, String>,
}

/// Token management modal. One block for GitHub, one block per
/// registered GitLab host (sourced from `Config::gitlab_hosts`). All
/// secrets live in the system keychain via `token_store`; this modal
/// reads/writes through `token:*` routes that the app handles.
///
/// `gitlab_hosts` is `(host, configured)` for each registered GitLab
/// host. Hosts come from `Config::gitlab_hosts` (auto-populated on
/// CI fetch); the configured flag comes from a `token_store` lookup.
pub fn token_modal(
    state: &TokenForm,
    selection: &Selection,
    github_set: bool,
    gitlab_hosts: &[(String, bool)],
) -> El {
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

    let mut sections: Vec<El> = vec![github_block];

    if gitlab_hosts.is_empty() {
        sections.push(form_item([
            form_label("GitLab"),
            form_description(
                "GitLab hosts appear here automatically when whisper-git \
                 sees a remote that points at one (e.g. `gitlab.com` or a \
                 self-hosted instance). Open a repo with a GitLab remote \
                 to register the host.",
            ),
        ]));
    } else {
        let mut rows: Vec<El> = Vec::with_capacity(gitlab_hosts.len());
        for (host, configured) in gitlab_hosts {
            rows.push(gitlab_host_row(state, selection, host, *configured));
        }
        sections.push(form_item([
            form_label("GitLab"),
            form_control(column(rows).gap(tokens::SPACE_2).width(Size::Fill(1.0))),
            form_description("One token per host. Per-host secrets go in the keychain."),
        ]));
    }

    let actions = row([spacer(), button("Done").key("modal:token:close").primary()])
        .align(Align::Center);
    sections.push(actions);

    let body = form(sections);

    overlays_panel(MODAL_TOKEN_KEY, "Manage tokens", [body])
}

/// Render one row of the GitLab section. Mirrors the GitHub row's
/// edit/idle split but scopes routes by host suffix (`token:gitlab:
/// edit:gitlab.com`, etc.) so the app can dispatch them correctly.
fn gitlab_host_row(
    state: &TokenForm,
    selection: &Selection,
    host: &str,
    configured: bool,
) -> El {
    let editing = state.gitlab_inputs.contains_key(host);
    let host_label = text(host.to_string())
        .label()
        .width(Size::Fixed(180.0));

    let controls: El = if editing {
        let buf = state.gitlab_inputs.get(host).cloned().unwrap_or_default();
        row([
            text_input(&buf, selection, &format!("token:gitlab:input:{host}"))
                .key(format!("token:gitlab:input:{host}"))
                .width(Size::Fill(1.0)),
            button("Save")
                .key(format!("token:gitlab:save:{host}"))
                .primary(),
            button("Cancel")
                .key(format!("token:gitlab:cancel:{host}"))
                .ghost(),
        ])
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .width(Size::Fill(1.0))
    } else {
        let status: El = if configured {
            badge("Configured").success()
        } else {
            badge("Not set").muted()
        };
        let mut children: Vec<El> = vec![
            status,
            spacer(),
            button(if configured {
                "Replace\u{2026}"
            } else {
                "Set\u{2026}"
            })
            .key(format!("token:gitlab:edit:{host}"))
            .ghost(),
        ];
        if configured {
            children.push(
                button("Clear")
                    .key(format!("token:gitlab:clear:{host}"))
                    .destructive(),
            );
        }
        row(children)
            .gap(tokens::SPACE_2)
            .align(Align::Center)
            .width(Size::Fill(1.0))
    };

    row([host_label, controls])
        .gap(tokens::SPACE_2)
        .align(Align::Center)
        .width(Size::Fill(1.0))
}
