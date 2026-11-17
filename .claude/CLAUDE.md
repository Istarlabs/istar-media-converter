# Claude Code Instructions

This repository is the `istar-media-converter` Cargo workspace: one binary crate per converter,
`crates/media-core` for what they share (tool identity, log, dialogs, gettext, config file, the ffmpeg
run). A new converter is a new crate; what two of them need goes into `media-core`, what only one
needs stays in that crate (`probe`, `analyze`, `plan` are video, `formats` is audio).
`crates/audio-convert` builds `istar-audio-converter`, a port of SavageCore's MIT project - it stays MIT
(the rest is GPL-3.0-or-later): keep both copyright lines in its headers and its `LICENSE`. Scripts call
that binary by name, so its name and CLI do not change lightly. `crates/image-to-pdf` keeps its
`single`/`multi` CLI. Most of this file, and the spec, describe `av1-convert`. `rust.py run|install` act on `av1-convert` unless
given `-p <crate>`.

`av1-convert` is a small Rust CLI: right-click a video in Dolphin, it analyzes the file with
ffprobe/ffmpeg, decides on deinterlacing and cropping by itself, and encodes AV1 on the GPU.

**The wiki page [AV1 convert](https://github.com/Istarlabs/istar-media-converter/wiki/AV1-convert) is the specification and the source of truth** (a clone
of the wiki, `istar-media-converter.wiki/` next to this repository, has it as `AV1-convert.md`). It
carries the CLI surface, the detection thresholds, the ffmpeg command lines, the safety rules and
the measured reference data the parsers are tested against. Read it before changing behaviour; when
this file and the spec disagree, the spec wins and this file gets fixed. Code cites it as `spec §N`, so
its section numbers do not move; a change of behaviour updates the page in the same go. The audio converter and
Image to PDF have wiki pages of their own.

## What this program must never do

Three rules out of the spec, repeated here because they are the ones a refactor quietly breaks:

- **Never touch the source file.** No `-y` anywhere, no in-place edit, no deletion - not even of a
  file the program itself wrote a moment ago, unless it is the `.partial`.
- **Write to `<stem>.av1.mkv.partial` first**, rename only after ffmpeg exits 0, remove the partial
  on failure and on Ctrl-C. An interrupted run leaves no half file that looks finished.
- **Verify before declaring success**: output duration within 1 s of the input, same number of audio
  streams. Otherwise the job failed, whatever ffmpeg's exit code said.

Started from Dolphin there is no terminal, so a failure nobody is told about is a failure nobody
sees. Every outcome goes to the log file *and* into the `kurodlg` window's details area, which stays
up under a Close button when the run ends - a notification only when there is no window (spec §7).
`notify::report` is the one way through; nothing calls `notify::log` for an outcome by itself.

## Rust standards

- **Everything in English**: identifiers, comments, log messages, CLI text, commit messages. What
  the user *reads in a dialog* is the exception: it is a literal inside `gettext()` and lives
  translated in `po/de/av1-convert.po` (spec §7.1, `.claude/skills/i18n`). The log file is not -
  `Scripts/logs.py` and the spec's examples read it in English. A message that goes to both is
  written twice, which is what `notify::report(dialog, notify, level, logged, shown)` takes.
  `python3 Scripts/i18n.py update` after touching any of those strings; the service menu's German
  labels stay in the `.desktop` file.
- Edition 2024, `rustfmt` defaults, `clippy` clean. `python3 Scripts/rust.py verify` is the gate.
- **The parsers and the decision logic are pure functions with unit tests.** `parse_idet_line`,
  `parse_cropdetect_line`, `parse_ffprobe`, "does this need deinterlacing", "which crop rectangle
  wins" - none of them may need a file or a subprocess to be tested. The sample lines and the
  measured outcomes in spec §4 and §10 are the test data; a threshold changed without a test that
  fails first is a guess.
- **A subprocess is an external interface, not a detail.** ffmpeg writes its analysis to *stderr*,
  `-progress` to stdout, and returns 0 while having produced nothing. Handle all three; never
  `unwrap()` on anything that came out of a child process, a path or a parse.
- `anyhow` for error context at the top level, plain `Result` below it. Context says what was being
  done to which file, because that string is what lands in the log and in the notification.
- Keep the module split from spec §9 (`probe`, `analyze`, `plan`, `encode`, `notify`, `text`). A
  file that grows a second responsibility gets split, not a banner comment.

## Comments

The default is no comment, and the budget is small. Two things earn one:

1. **Knowledge from outside the repository** - what a foreign tool actually does. *"ffmpeg writes
   idet's result to stderr, and the multi-frame line is the one to read"*, *"cropdetect needs about
   ten frames to settle, so the first ten outputs of each sample are dropped"*, *"`-b:v 0` is
   required or a default bitrate cap silently overrides `-cq`"*. None of that is derivable from the
   code.
2. **Meaning and choice where the code cannot say it** - why *this* threshold, bound or tolerance.
   `0.30` without provenance reads as a guess; with `(spec §4.1, measured on the Film A rip)` it
   reads as a decision.

What earns nothing: restating the line below, narrating the next step, defending a decision nobody
challenged, history git already has.

`no-decorative-comments.sh` refuses banner comments (`// ======`, `// ---- x ----`) and `/* */`
blocks in `.rs` files. A section that needs a banner needs a module.

**The hooks live in the shared devscripts checkout**, not in this repository - `.claude/settings.json`
points at `${DEVSCRIPTS:-$HOME/dev/tools/devscripts}/hooks/`. Edit them there, once, and run
`bash "$DEVSCRIPTS/hooks/no-decorative-comments.test.sh"` after touching one. The local copies were
deleted on 2026/09/21; they had a `cargo` rule the shared one lacked, which is now in the shared one
with tests, and they were missing every fix the shared one had picked up since.

## Repository scripts - use them before writing a command chain

`Scripts/` holds the recurring chores. Reach for these first; rebuilding them inline as
`cargo … | grep …` is slower, and asks for permission on every run because each ad-hoc command line
is unique. `prefer-repo-scripts.sh` in the shared checkout enforces it.

| Task | Command |
| --- | --- |
| Clippy, tests, rustfmt, licence lines | `python3 Scripts/rust.py verify` |
| Warnings by lint, or every hit of one lint | `python3 Scripts/rust.py warnings [--code clippy::x]` |
| Tests, with the failed assertion | `python3 Scripts/rust.py test [NAME] [--details]` |
| Dependencies, advisories, updates, one crate | `python3 Scripts/rust.py packages [--vulnerable\|--outdated\|--info anyhow]` |
| Toolchain, editions, what is pinned | `python3 Scripts/rust.py toolchain`, `… crates` |
| Run it against a real file | `python3 Scripts/rust.py run -- --analyze-only FILE` |
| Install to ~/.local/bin + the Dolphin menu | `python3 Scripts/rust.py install` |
| Where a symbol is used (and whether at all) | `python3 Scripts/find.py Symbol [--lines] [--crates]` |
| Where a piece of *text* appears, any file type | `python3 Scripts/find.py "cropdetect" --text` |
| What a file declares | `python3 Scripts/find.py --types --in crates/av1-convert/src` |
| Branch, commit, push, issues, PRs, merges | `python3 Scripts/repo.py --help` |
| What a run logged | `python3 Scripts/logs.py [--grep X] [--level Warn] [--date D]` |
| SPDX licence headers | `python3 Scripts/spdx.py check\|apply` |
| Translations: extract, merge, what is missing | `python3 Scripts/i18n.py update\|status` |

**`find.py` before writing anything new.** The symbol search matches whole words, so it answers
"where is X used" only once you know X. Looking for a *concept* - "do we already have something for
cropping" - is `find.py crop --text`, and `--crates` searches the sources of the crates this project
resolves, which is how a question about `anyhow` or `clap` gets answered without guessing.

**`rust.py verify` before every commit.** It separates the warnings on the lines this branch changed
from the ones that were already there, so a clippy lint in a file you happened to touch is not read
as yours.

**Never hand-build a `git` or `gh` line.** `repo.py` covers branching, committing (with the
`Co-Authored-By` trailer), pushing, issues, labels and PRs, and carries the guards: no branching off
a dirty tree, no committing to `master` without `--allow-master`, no merging what GitHub does not
report `MERGEABLE`.

**Write body files with the Write tool, never a heredoc.** `repo.py` takes commit messages, issue
bodies and PR bodies via `--file`. A heredoc makes the whole command line unique, so no allow rule
can ever match it - which defeats the point of having the script.

**A missing action on an existing script? Add it - no need to ask.** When a script owns its area and
simply cannot do the thing, extend it rather than hand-rolling the step. A brand-new script for a
new kind of chore deserves a heads-up first.

## This machine

- **The shell aliases `ls` to `eza` and `cat` to `bat`.** Escape them as `\ls` and `\cat` whenever
  the output is parsed or piped.
- `gh` has no `--no-pager`; `git` takes it only before the subcommand. Use `GH_PAGER=cat` /
  `GIT_PAGER=cat` - `repo.py` sets both already.
- **Do not prefix commands with `cd`.** The repository is the working directory and it persists
  between calls; every script derives its paths from its own location. A hook refuses it.
- ffmpeg 8.1.2 (Fedora build) with nvenc and libsvtav1, ffprobe present, RTX 4070 Ti Super for
  `av1_nvenc`. `ffmpeg`/`ffprobe` are deliberately *not* wrapped by a script - measuring a real file
  by hand is the point.

## The Dolphin side

A menu is a definition, `dolphin/<name>.menu.toml` (mime types, submenu, a list of actions; the
format is in devscripts' `servicemenu.py`), listed under `[rust.service_menu]` in `Scripts/project.toml`.
`rust.py install` writes `~/.local/share/kio/servicemenus/<name>.desktop` from each one, marks it
executable and runs `kbuildsycoca6` - without that last step KDE does not notice a new or changed
menu. `install --menu-only` is the quick way after editing a definition.

**There is no finished `.desktop` file to keep in step.** Editing an installed file by hand is how a
fix gets lost: the next `install` simply overwrites it. The RPM renders the same definitions for
`/usr/bin` (`packaging/rpm-stage.sh`).
The audio converter's menu is the exception to the definition file: its labels carry the configured
quality, so `istar-audio-converter --install-menu DIR` (`menu.rs`) renders it, `--configure` renders
it again after a save, and `rust.py install` runs the former through `generators` in `project.toml`.
`rust.py install -p audio-convert` puts that binary in `~/.local/bin` first.

**The audio crate has already solved the KDE parts.** `crates/audio-convert` runs a
`kdialog --progressbar` over D-Bus with `zbus` - including the two traps that cost a day there:
`kdialog --progressbar` prints `service objectpath` as *two* fields, and a failed progress call must
not be read as "the user cancelled". `media-core::notify` is the shared form of that, with kurodlg.

## Workflow

Issue → branch → commit → PR, through `repo.py`, and everything on GitHub written in English. For a
genuinely small self-contained change, ask whether it goes straight to `master` rather than
assuming the waiver.

Branch names `feat/…`, `fix/…`, `chore/…`; conventional commit subjects
(`feat(analyze): detect telecine`). The commit body says *why*, not what the diff already shows.
