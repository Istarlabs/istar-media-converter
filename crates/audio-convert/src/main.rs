// SPDX-FileCopyrightText: 2026 SavageCore
// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: MIT

//! KDE/Dolphin service menu audio converter backend (Plasma 6 only). A Rust port of
//! <https://github.com/SavageCore/dolphin-audio-converter>.

mod config;
mod convert;
mod formats;
mod menu;
mod ui;

use std::path::PathBuf;

use clap::Parser;
use media_core::app::{self, App};

use crate::convert::{convert_files, extract_files, require_ffmpeg, run_extract_convert};
use crate::formats::Format;
use crate::ui::kdialog;

/// KDE Dolphin service menu audio converter backend (Plasma 6)
#[derive(Parser)]
#[command(group = clap::ArgGroup::new("mode")
    .args(["format", "configure", "install_menu", "extract", "extract_convert"])
    .multiple(false))]
struct Cli {
    /// Target format: mp3, ogg, flac, wav, m4a, opus, alac
    #[arg(long)]
    format: Option<String>,

    /// Open the quality configuration dialog
    #[arg(long)]
    configure: bool,

    /// Write the Dolphin service menu for the binary installed in DIR, with the configured quality
    #[arg(long, value_name = "DIR")]
    install_menu: Option<PathBuf>,

    /// With --install-menu: write the menu to FILE, with the default qualities rather than this user's (for a package)
    #[arg(long, value_name = "FILE", requires = "install_menu")]
    menu_file: Option<PathBuf>,

    /// Extract the audio stream without re-encoding
    #[arg(long)]
    extract: bool,

    /// Pick a target format, then extract and convert
    #[arg(long)]
    extract_convert: bool,

    /// Input files
    files: Vec<String>,
}

fn require_files(files: &[String]) {
    if files.is_empty() {
        kdialog(&["--error", "No input files provided."]);
        std::process::exit(1);
    }
    if !require_ffmpeg() {
        std::process::exit(1);
    }
}

fn main() {
    app::set(App {
        name: "istar-audio-converter",
        env_prefix: "ISTAR_AUDIO_CONVERTER",
        title: "Audio Converter".to_string(),
        icon: "audio-x-generic",
    });

    let cli = Cli::parse();

    if cli.configure {
        config::run_configure();
        return;
    }

    if let Some(directory) = cli.install_menu {
        // A package ships the defaults: the builder's own --configure choices are not the users'.
        let (quality, path) = match cli.menu_file {
            Some(file) => (config::Config::new(), file),
            None => (config::load_config(), menu::user_menu_path()),
        };

        match menu::write(&quality, &directory, &path) {
            Ok(path) => println!("installed {}", path.display()),
            Err(error) => {
                eprintln!("writing the service menu: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if cli.extract {
        require_files(&cli.files);
        extract_files(&cli.files);
        return;
    }

    if cli.extract_convert {
        require_files(&cli.files);
        run_extract_convert(&cli.files);
        return;
    }

    let Some(fmt_arg) = cli.format.map(|f| f.to_lowercase()) else {
        use clap::CommandFactory;
        let _ = Cli::command().print_help();
        std::process::exit(1);
    };

    let Some(fmt) = Format::from_key(&fmt_arg) else {
        let supported: Vec<&str> = Format::ALL.iter().map(|f| f.key()).collect();
        let msg = format!(
            "Unknown format: '{}'\nSupported: {}",
            fmt_arg,
            supported.join(", ")
        );
        kdialog(&["--error", msg.as_str()]);
        std::process::exit(1);
    };

    require_files(&cli.files);

    let cfg = config::load_config();
    convert_files(&cli.files, fmt, &config::quality_for(&cfg, fmt));
}
