#!/bin/sh
# SPDX-FileCopyrightText: 2026 Pascal Klose
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Lays out the package below STAGE as it will sit under /: the three binaries, the Dolphin menus and
# the translations. Called by devscripts' rpm.py ([rpm] build in Scripts/project.toml) after
# `cargo build --release`.
#
#     sh packaging/rpm-stage.sh STAGE
set -eu

stage=$1
menus="$stage/usr/share/kio/servicemenus"

for binary in av1-convert istar-audio-converter image-to-pdf; do
    install -Dm755 "target/release/$binary" "$stage/usr/bin/$binary"
done

# The AV1 and Image-to-PDF menus come from their definitions, rendered by devscripts' servicemenu.py
# (found the way the Scripts/ shims find devscripts). The audio menu is written by its own binary, with
# the default qualities: the builder's --configure choices are nobody else's.
# No executable bit: KDE asks for one only from menus in a user's own directory, and rpmbuild strips it anyway.
devscripts=${DEVSCRIPTS:-$HOME/dev/tools/devscripts}
[ -d "$devscripts/shared" ] || devscripts=.devscripts
mkdir -p "$menus"

for definition in dolphin/*.menu.toml; do
    python3 "$devscripts/shared/lib/servicemenu.py" "$definition" --bin /usr/bin \
        > "$menus/$(basename "$definition" .menu.toml).desktop"
done

target/release/istar-audio-converter --install-menu /usr/bin --menu-file "$menus/istar-audio-converter.desktop" >/dev/null

# The binaries look for their catalogues next to their own prefix: /usr/bin -> /usr/share/locale.
for catalogue in po/*/av1-convert.po; do
    language=$(basename "$(dirname "$catalogue")")
    install -d "$stage/usr/share/locale/$language/LC_MESSAGES"
    msgfmt --check-format -o "$stage/usr/share/locale/$language/LC_MESSAGES/av1-convert.mo" "$catalogue"
done
