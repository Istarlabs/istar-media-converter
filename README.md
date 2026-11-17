# istar-media-converter

Converters for Dolphin's context menu, one binary per job, in one Cargo workspace:

| Crate | Binary | What it does |
| --- | --- | --- |
| `crates/av1-convert` | `av1-convert` | Video to AV1 on the GPU, analysis included (described below) |
| `crates/audio-convert` | `istar-audio-converter` | Audio to MP3, OGG, FLAC, WAV, M4A, Opus or ALAC, or extract a stream - [wiki](https://github.com/Istarlabs/istar-media-converter/wiki/Audio-converter) |
| `crates/image-to-pdf` | `image-to-pdf` | One image to a PDF, or several to one PDF, through ImageMagick: `image-to-pdf single FILE`, `image-to-pdf multi FILE...`; an existing PDF is never overwritten - [wiki](https://github.com/Istarlabs/istar-media-converter/wiki/Image-to-PDF) |
| `crates/media-core` | - | What they share: tool identity, log, dialogs and notifications, gettext, the config file, the ffmpeg run |

A new converter is a new crate beside these, using `media-core` for the parts that are not its own.

## av1-convert

Right-click a video in Dolphin, choose *Convert to AV1*: the tool probes the file with ffprobe,
samples it with ffmpeg's `idet` and `cropdetect`, decides on deinterlacing and cropping by itself,
and encodes with `av1_nvenc`. Audio, subtitles and chapters are copied; the source is never touched.

```
av1-convert [--analyze-only] [--cq N] [--audio copy|opus] [--audio-langs deu,eng]
            [--subtitle-langs auto|all|none|deu] [--10bit] [--crop-anyway] [--ask]
            [--verbose] [--notify] <FILE>...
av1-convert --configure
```

The service menu has four entries: convert, convert with the settings picked in a window first
(`--ask`: a preset to start from, then quality, Opus and 10 bit to adjust), analyze only, and
*Default settings…* (`--configure`), a form over `~/.config/av1-convert/config.toml` that creates the
file on its first save. The keys are the long options without their dashes (`cq = 24`, `audio-langs = "deu"`);
anything typed beats the file, and a file that does not parse is reported and ignored.

Interlaced material is deinterlaced, telecined NTSC gets an inverse telecine, and with `--notify`
on there is one [`kurodlg`](https://github.com/Istarlabs/kurodlg) window for the whole selection: a progress bar with a
Cancel button, and a details area that collects what happened to every file. It stays up under a
*Close* button when the run is over, so nothing has to be caught as a passing notification. Without
that binary the conversion runs on, and the outcomes go to notifications instead.

The dialogs follow the session's language (`po/`, gettext - German is translated); the log file at
`~/.local/state/av1-convert/av1-convert.log` and the terminal output stay English.

The [wiki](https://github.com/Istarlabs/istar-media-converter/wiki) has a page per converter. [AV1 convert](https://github.com/Istarlabs/istar-media-converter/wiki/AV1-convert) is the specification: detection
thresholds, the ffmpeg command lines, the safety rules and the measured reference data.

## Build and install

The scripts in `Scripts/` come from [devscripts](https://github.com/Istarlabs/devscripts), pinned as a submodule: clone
with `git clone --recurse-submodules`, or run `git submodule update --init` in an existing clone.

```bash
python3 Scripts/rust.py verify     # clippy, tests, rustfmt, licence headers - the whole workspace
python3 Scripts/rust.py install    # av1-convert: ~/.local/bin/av1-convert, the .mo files, the Dolphin menu
python3 Scripts/i18n.py update     # after touching any string the user reads
```

Needs ffmpeg 8 with `av1_nvenc`, ffprobe, an NVIDIA GPU with AV1 encoding, and a Plasma 6 session
for the menu. `Scripts/` holds the repository's chores - `rust.py`, `find.py`, `repo.py`, `logs.py`,
`spdx.py`, each with `--help`.

## RPM

```bash
python3 Scripts/rpm.py build       # ~/.cache/devscripts/rpm/istar-media-converter-<version>-<timestamp>.x86_64.rpm
python3 Scripts/rpm.py publish     # the same, sent to an RPM repository over ssh
```

One package for the workspace: the three binaries in `/usr/bin`, their Dolphin menus in `/usr/share/kio/servicemenus`
(the audio menu with the default qualities - `--configure` gives a user a copy of their own with theirs) and the
translations. `packaging/rpm-stage.sh` lays it out; `[rpm]` in `Scripts/project.toml` names what it needs at run time
(ffmpeg, ffprobe, ImageMagick, kdialog, notify-send; kurodlg and konsole recommended). `publish` needs a repository:
`[rpm] repository = "host:path"` or `DEVSCRIPTS_RPM_REPOSITORY`. A build installed into `~/.local` before - its
binaries and its menus in `~/.local/share/kio/servicemenus` - stays in front of the package and is removed by hand.

## Licence

istar-media-converter is licensed under the [GNU General Public License v3.0 or later](LICENSE.md), except
`crates/audio-convert`: a port of SavageCore's
[dolphin-audio-converter](https://github.com/SavageCore/dolphin-audio-converter), which stays under the MIT licence
of the original ([LICENSE](crates/audio-convert/LICENSE)).
