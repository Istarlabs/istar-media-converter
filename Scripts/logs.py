#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Pascal Klose
# SPDX-License-Identifier: GPL-3.0-or-later

"""One entry point for reading av1-convert's own log.

Started from Dolphin's context menu there is no terminal, so the log file is the only record of what
a run decided and why. It lives under `$XDG_STATE_HOME/av1-convert/av1-convert.log`
(`~/.local/state/av1-convert/av1-convert.log`), which is the path everyone gets wrong, and a
hand-built `tail`/`grep` line is refused by this repository's tooling rule anyway.

    python3 Scripts/logs.py                     the last 50 entries
    python3 Scripts/logs.py -n 200              more of them
    python3 Scripts/logs.py --all               all of them
    python3 Scripts/logs.py --date 2026-09-20   one day
    python3 Scripts/logs.py --grep Film A     entries whose text contains this (case-insensitive)
    python3 Scripts/logs.py --level Warn        this level and above
    python3 Scripts/logs.py --list              which days the file holds, with entry counts
    python3 Scripts/logs.py --path              just the file this would read

Filtering works on *entries*, not lines: one failed encode logs the plan, the full ffmpeg command
line and the last 30 lines of ffmpeg's stderr, and only the first of those carries a timestamp.
Matching line by line would print a stack of ffmpeg noise without the message that explains it, or
the message without the noise that shows what went wrong.

This is also where the log *format* is pinned down. The Rust side writes one entry per event as

    [2026-09-20 21:44:03] INFO  Film A: no deinterlace (6 %), crop=720:416:0:80, cq 28

and every following line of the same entry is indented. AV1_CONVERT_LOG overrides the file, for a
log copied off another machine.

Exit code is non-zero when the file does not exist yet, so it can gate a chain.
"""
import argparse
import collections
import os
import re
import sys

STATE_HOME = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
DEFAULT_LOG = os.path.join(STATE_HOME, "av1-convert", "av1-convert.log")

# Worst last, because `--level Warn` means Warn and everything worse - the only reading of "show me
# the warnings" that does not hide the errors underneath them.
LEVELS = ["Trace", "Debug", "Info", "Warn", "Error"]

# The layout starts every entry with "[2026-09-20 21:44:03] INFO ". A line that does not match is a
# continuation of the entry above it.
ENTRY_START = re.compile(r"^\[(?P<time>\d{4}-\d{2}-\d{2})[^\]]*\]\s+(?P<level>[A-Z]+)\b")


def fail(message):
    sys.stderr.write(f"refused: {message}\n")
    sys.exit(1)


def log_file(override):
    """The file to read, or a refusal naming what was looked for."""
    path = override or os.environ.get("AV1_CONVERT_LOG") or DEFAULT_LOG

    if not os.path.isfile(path):
        fail(f"no log at {path} - has av1-convert run on this machine yet? "
             "AV1_CONVERT_LOG points this at another file.")

    return path


def read_entries(path):
    """
    The file as entries: each is its first line plus every continuation line under it.

    A file that starts mid-entry - one that was truncated, or a fragment pasted from elsewhere -
    would otherwise drop its opening lines, so anything before the first timestamp becomes an entry
    of its own, at no level and on no day.
    """
    entries = []

    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.rstrip("\n")
            match = ENTRY_START.match(line)

            if match or not entries:
                entries.append({"level": match.group("level").title() if match else "",
                                "date": match.group("time") if match else "",
                                "lines": [line]})
            else:
                entries[-1]["lines"].append(line)

    return entries


def matching(entries, term, level, date):
    if date:
        entries = [entry for entry in entries if entry["date"] == date]

    if level:
        minimum = LEVELS.index(level)
        # An entry whose level this script does not know stays in: a log it cannot classify is the
        # last thing to hide from someone who came here looking for a problem.
        entries = [entry for entry in entries
                   if entry["level"] not in LEVELS or LEVELS.index(entry["level"]) >= minimum]

    if term:
        needle = term.lower()
        entries = [entry for entry in entries
                   if any(needle in line.lower() for line in entry["lines"])]

    return entries


def command_list(path):
    entries = read_entries(path)
    days = collections.Counter(entry["date"] for entry in entries if entry["date"])

    print(f"{path}  {os.path.getsize(path) / 1024:.1f} KiB  {len(entries)} entries\n")

    for day, count in sorted(days.items()):
        levels = collections.Counter(entry["level"] for entry in entries if entry["date"] == day)
        worst = ", ".join(f"{level} {levels[level]}" for level in LEVELS if levels[level])
        print(f"  {day}  {count:>5} entries   {worst}")


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--date", help="Only this day, as 2026-09-20")
    parser.add_argument("-n", "--lines", type=int, default=50,
                        help="How many entries to show (default: 50)")
    parser.add_argument("--all", action="store_true", help="Show every matching entry")
    parser.add_argument("--grep", help="Only entries containing this text, case-insensitive")
    parser.add_argument("--level", choices=LEVELS, help="This level and above")
    parser.add_argument("--list", action="store_true", help="Which days the file holds, with entry counts")
    parser.add_argument("--path", action="store_true", help="Print the file that would be read")
    parser.add_argument("--file", help="Read this file instead of the one in ~/.local/state")

    arguments = parser.parse_args()
    path = log_file(arguments.file)

    if arguments.path:
        print(path)

        return

    if arguments.list:
        command_list(path)

        return

    entries = matching(read_entries(path), arguments.grep, arguments.level, arguments.date)

    if not entries:
        print(f"{path}: nothing matches", file=sys.stderr)

        return

    shown = entries if arguments.all else entries[-arguments.lines:]
    hidden = len(entries) - len(shown)

    # Ahead of the entries, not after them: stderr and stdout interleave by buffer, so a note
    # printed last lands anywhere, and as a header it reads as what it is.
    if hidden:
        print(f"… {hidden} earlier matching entr{'y' if hidden == 1 else 'ies'} not shown, "
              f"of {len(entries)}\n", file=sys.stderr)

    for entry in shown:
        print("\n".join(entry["lines"]))


if __name__ == "__main__":
    main()
