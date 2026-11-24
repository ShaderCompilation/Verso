#!/bin/sh
# Reset Verso's user data on the Kindle so the next launch runs first-time setup.
# Deletes settings, library index, reading state, annotations, covers, exports,
# navigation state and the crengine cache. Keeps the installed app (bin, fonts,
# wordforms, cre/ resources, update/), the book folders and all dictionaries
# (dictionaries/, hunspell/, dictionary-cache/, dictionaries.json,
# dictionary-folders.json). Only touches /mnt/us/verso.
set -eu
V=/mnt/us/verso
D=$V/data

ssh kindle "
set -eu
[ \"\$(lipc-get-prop com.lab126.powerd state)\" = active ] || { echo 'Kindle is not awake' >&2; exit 1; }
if pgrep -f '$V/verso( |\$)' >/dev/null; then echo 'Verso is running; quit it first' >&2; exit 1; fi
cd $D
rm -rf annotations books covers exports
rm -f settings.json library.json home.json opened.json library-navigation.json reading-speed.json
rm -rf $V/cache/*
echo 'kept in data:'; ls
"
