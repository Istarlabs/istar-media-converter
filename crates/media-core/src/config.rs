// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::app;

/// `$<PREFIX>_CONFIG`, else `$XDG_CONFIG_HOME/<name>/config.toml`.
pub fn path() -> PathBuf {
    let app = app::get();

    if let Some(override_path) = env::var_os(format!("{}_CONFIG", app.env_prefix)) {
        return PathBuf::from(override_path);
    }

    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));

    base.join(app.name).join("config.toml")
}

/// The file, or an empty config when there is none. `Err` carries why a file that is there could
/// not be used; the caller goes on without it, because a broken config must not stop a
/// conversion, but says so where the run is being watched.
pub fn load<T: DeserializeOwned + Default>() -> Result<T, String> {
    let path = path();

    match fs::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|reason| format!("{}: {reason}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The config file, written whole. It goes to a `.partial` next to it first and is renamed over the
/// original, so a failed write never leaves half a config where the next run reads it.
pub fn save(text: &str) -> Result<PathBuf, String> {
    let path = path();

    write_whole(&path, text).map_err(|error| format!("{}: {error}", path.display()))?;

    Ok(path)
}

fn write_whole(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");

    fs::write(&partial, text)?;
    fs::rename(&partial, path)
}

/// A string as a TOML value, quotes and escapes included.
pub fn quote(text: &str) -> String {
    toml::Value::String(text.to_string()).to_string()
}

/// toml's own Display draws the offending line with a caret under it - right for a terminal, too
/// much for one line in a dialog. The message and the line number are what is kept.
pub fn parse<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    toml::from_str(text).map_err(|error| match error.span() {
        Some(span) => format!(
            "line {}: {}",
            text[..span.start].matches('\n').count() + 1,
            error.message()
        ),
        None => error.message().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_string_reads_back_as_it_went_in() {
        for text in ["deu,eng", "say \"hi\"", "back\\slash", "tab\there", "ä…"] {
            let table: toml::Table = parse(&format!("key = {}", quote(text))).unwrap();

            assert_eq!(table["key"].as_str(), Some(text));
        }
    }

    #[test]
    fn the_file_is_replaced_whole_and_no_partial_stays() {
        let directory = env::temp_dir().join(format!("media-core-config-{}", std::process::id()));
        let path = directory.join("nested").join("config.toml");

        write_whole(&path, "cq = 20\n").unwrap();
        write_whole(&path, "cq = 24\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "cq = 24\n");
        assert!(!path.with_file_name("config.toml.partial").exists());

        let _ = fs::remove_dir_all(&directory);
    }
}
