// SPDX-FileCopyrightText: 2026 SavageCore
// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: MIT

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::formats::{Format, configurable_formats};
use crate::menu;
use crate::ui::{kdialog, notify};

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn config_dir() -> PathBuf {
    home_dir().join(".config").join("istar-audio-converter")
}

fn config_path() -> PathBuf {
    config_dir().join("config.txt")
}

/// Per-format quality setting; every format always has an entry.
pub type Config = HashMap<Format, String>;

pub fn load_config() -> Config {
    parse_config(&fs::read_to_string(config_path()).unwrap_or_default())
}

/// The defaults with the file's `key=value` lines laid over them. A value that is not in the
/// format's quality table (a hand-edited typo) is ignored rather than passed on to ffmpeg.
fn parse_config(text: &str) -> Config {
    let mut cfg: Config = Format::ALL
        .into_iter()
        .map(|f| (f, f.default_quality().to_string()))
        .collect();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            if let Some(fmt) = Format::from_key(k.trim())
                && fmt.quality_options().contains(&v)
            {
                cfg.insert(fmt, v.to_string());
            }
        }
    }
    cfg
}

fn save_config(cfg: &Config) {
    let _ = fs::create_dir_all(config_dir());
    let mut text = String::new();
    for fmt in Format::ALL {
        if let Some(v) = cfg.get(&fmt) {
            text.push_str(&format!("{}={}\n", fmt.key(), v));
        }
    }
    let _ = fs::write(config_path(), text);
}

/// The configured quality for a format (falls back to the format default).
pub fn quality_for(cfg: &Config, fmt: Format) -> String {
    cfg.get(&fmt)
        .cloned()
        .unwrap_or_else(|| fmt.default_quality().to_string())
}

pub fn run_configure() {
    let mut cfg = load_config();
    let configurable = configurable_formats();

    let mut menu_args: Vec<String> = vec![
        "--title".into(),
        "Audio Converter - Configure".into(),
        "--menu".into(),
        "Select a format to configure:".into(),
    ];
    for f in &configurable {
        menu_args.push(f.key().to_string());
        menu_args.push(format!(
            "{}   (currently: {})",
            f.display(),
            quality_for(&cfg, *f)
        ));
    }
    let r = kdialog(&menu_args.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    if r.code != 0 {
        return;
    }
    let Some(chosen_fmt) = Format::from_key(r.stdout.trim()) else {
        return;
    };
    if !configurable.contains(&chosen_fmt) {
        return;
    }

    let options = chosen_fmt.quality_options();
    let current = quality_for(&cfg, chosen_fmt);

    let mut q_args: Vec<String> = vec![
        "--title".into(),
        format!("Configure {}", chosen_fmt.display()),
        "--menu".into(),
        format!("Output quality for {}:", chosen_fmt.display()),
    ];
    for opt in options {
        let marker = if *opt == current { "  ✔" } else { "" };
        q_args.push((*opt).to_string());
        q_args.push(format!(
            "{} - {}{}",
            opt,
            chosen_fmt.quality_desc(opt),
            marker
        ));
    }
    let r2 = kdialog(&q_args.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    if r2.code != 0 {
        return;
    }
    let chosen_q = r2.stdout.trim().to_string();
    if !options.contains(&chosen_q.as_str()) {
        return;
    }

    cfg.insert(chosen_fmt, chosen_q.clone());
    save_config(&cfg);

    // Where this binary sits is where the menu it was started from points at.
    if let Some(directory) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
    {
        menu::refresh(&cfg, directory);
    }

    notify(
        "Audio Converter - Settings saved",
        &format!("{} quality → {}", chosen_fmt.display(), chosen_q),
        "configure",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_leaves_the_defaults() {
        let cfg = parse_config("");

        assert_eq!(quality_for(&cfg, Format::Mp3), "V0");
        assert_eq!(quality_for(&cfg, Format::Flac), "lossless");
    }

    #[test]
    fn the_file_overrides_what_it_names() {
        let cfg = parse_config("# comment\nmp3 = V2\n\nopus=64k\n");

        assert_eq!(quality_for(&cfg, Format::Mp3), "V2");
        assert_eq!(quality_for(&cfg, Format::Opus), "64k");
        assert_eq!(quality_for(&cfg, Format::Ogg), "Q6");
    }

    #[test]
    fn a_value_outside_the_quality_table_is_ignored() {
        let cfg = parse_config("mp3=V9\nogg=high\nunknown=1\n");

        assert_eq!(quality_for(&cfg, Format::Mp3), "V0");
        assert_eq!(quality_for(&cfg, Format::Ogg), "Q6");
    }
}
