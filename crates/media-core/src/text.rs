// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::path::PathBuf;

use gettextrs::LocaleCategory;

/// Bind the catalogue. Everything the user reads goes through `gettext()`; the log file does not,
/// because it is read next to the source and by `Scripts/logs.py`.
///
/// The domain is the tool's name: the .mo file is
/// `<prefix>/share/locale/<language>/LC_MESSAGES/<domain>.mo`, and a name that does not match the one
/// the catalogue was installed under fails silently - every string simply stays English.
pub fn init(domain: &str) {
    // Without setlocale() the process stays in the "C" locale whatever LANG says, and gettext
    // answers in English no matter which catalogue is installed. It is unsafe because the C call
    // is not thread-safe; this is the first statement of main(), where nothing else runs yet.
    unsafe {
        let _ = gettextrs::setlocale(LocaleCategory::LcAll, "");
    }

    if let Some(directory) = locale_directory() {
        let _ = gettextrs::bindtextdomain(domain, directory);
    }

    let _ = gettextrs::bind_textdomain_codeset(domain, "UTF-8");
    let _ = gettextrs::textdomain(domain);
}

/// Where the catalogues are, derived from the binary rather than fixed at build time: this installs
/// into `~/.local/bin`, so `~/.local/share/locale` is the prefix that goes with it. A `cargo run`
/// out of `target/` finds nothing there and falls back to English, which is what a terminal run
/// wants anyway.
fn locale_directory() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    let prefix = executable.parent()?.parent()?;
    let directory = prefix.join("share").join("locale");

    directory.is_dir().then_some(directory)
}

/// One message in both spellings: English for the log file, the session's language for the window.
///
/// The English text is written twice at every call site - once plainly, once as the `gettext()`
/// argument - and that is not redundancy to be factored out: extraction is static, so the argument
/// has to stay a literal or the string never reaches a translator.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Message {
    pub logged: String,
    pub shown: String,
}

impl Message {
    pub fn new(english: &str, translated: String) -> Message {
        Message {
            logged: english.to_string(),
            shown: translated,
        }
    }

    /// Both templates, filled with the same values.
    pub fn filled(english: &str, translated: String, values: &[(&str, &str)]) -> Message {
        Message {
            logged: fill(english, values),
            shown: fill(&translated, values),
        }
    }
}

/// Fills `{name}` placeholders after the translation. `format!` needs a literal, and a catalogue
/// entry is a string that arrives at runtime - so the pieces are put in here, which also lets a
/// translation reorder them.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = template.to_string();

    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value);
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_every_placeholder_wherever_it_sits() {
        assert_eq!(
            fill(
                "{file}: {change}",
                &[("file", "a.mkv"), ("change", "-71 %")]
            ),
            "a.mkv: -71 %"
        );

        // German puts the pieces in another order, which is the reason the filling happens after
        // the lookup rather than in a format! at the call site.
        assert_eq!(
            fill(
                "{change} für {file}",
                &[("file", "a.mkv"), ("change", "-71 %")]
            ),
            "-71 % für a.mkv"
        );
    }

    #[test]
    fn leaves_what_it_was_not_given() {
        assert_eq!(fill("{file}: {change}", &[]), "{file}: {change}");
        assert_eq!(
            fill("no placeholder", &[("file", "a.mkv")]),
            "no placeholder"
        );
    }
}
