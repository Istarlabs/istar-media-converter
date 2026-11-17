// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::OnceLock;

/// Which tool is running. The log file, the config file, their environment overrides and the
/// notification's sender all derive from it, so a second binary changes one value, not four paths.
#[derive(Debug, Clone)]
pub struct App {
    /// Directory and file stem under `$XDG_STATE_HOME` and `$XDG_CONFIG_HOME`, and the gettext domain.
    pub name: &'static str,

    /// Prefix of `<PREFIX>_LOG` and `<PREFIX>_CONFIG`, which point the tool at another file.
    pub env_prefix: &'static str,

    /// The notification summary, translated by the tool: the string has to be a `gettext()` literal in
    /// the tool's own sources or extraction never sees it.
    pub title: String,

    /// The notification icon of an informational outcome; warnings and errors have their own.
    pub icon: &'static str,
}

static APP: OnceLock<App> = OnceLock::new();

/// Set once, right after `text::init`. A second call is ignored.
pub fn set(app: App) {
    let _ = APP.set(app);
}

/// What the tool set, or a neutral identity when it set nothing - which is what unit tests run as.
pub fn get() -> &'static App {
    APP.get_or_init(|| App {
        name: "media-converter",
        env_prefix: "MEDIA_CONVERTER",
        title: "Media conversion".to_string(),
        icon: "video-x-generic",
    })
}
