#!/usr/bin/env bash
# A/B check for the Linux resize bug (user report: window hard to resize,
# escapes the screen).
#
# Root cause: linux_layout sized the GtkFixed children with
# set_size_request(current window size); GTK3 folds child requests into the
# toplevel minimum, so the window minimum tracked its current size — grow
# only. The smoking gun is the X11 WM_NORMAL_HINTS min_width/min_height,
# which must stay at the configured 480x400 floor and never follow the
# window's current size.
#
# Runs the OLD binary (the published 0.2.17 tarball, Debian 12 build) and
# the NEW build back to back under WSLg's XWayland and prints each window's
# size hints. Usage, from Git Bash:
#
#   MSYS_NO_PATHCONV=1 wsl.exe -d bzdium-deb12 -u root -e bash \
#     /mnt/e/dev/bzdium/build-helpers/resize-ab-test.sh
set -u

OLD_DIR=/root/ab/old
NEW_DIR=/root/ab/new
export GDK_BACKEND=x11
export WLR_RENDERER=pixman 2>/dev/null || true

rm -rf "$OLD_DIR" "$NEW_DIR"
mkdir -p "$OLD_DIR" "$NEW_DIR"
tar xzf /mnt/e/dev/bzdium/dist-release-0.2.17-deb12/bzdium-0.2.17-linux-x64.tar.gz -C "$OLD_DIR"
cp /root/bzdium-target/release/bzdium "$NEW_DIR/"

run_case() { # $1=dir $2=label
    (cd "$1" && ./bzdium >/tmp/ab-$2.log 2>&1 &)
    for _ in $(seq 1 30); do
        sleep 1
        id=$(xdotool search --onlyvisible --name bzdium | head -1)
        [ -n "$id" ] && break
    done
    if [ -z "${id:-}" ]; then
        echo "== $2: WINDOW NOT FOUND (see /tmp/ab-$2.log)"; return
    fi
    echo "== $2: window id $id"
    xdotool getwindowgeometry --window "$id" | grep -i 'geometry'
    xprop -id "$id" WM_NORMAL_HINTS | tr -s ' ' | fold -w 100 | head -3
    # now try to shrink below the old poisoned minimum
    xdotool windowsize "$id" 700 500
    sleep 2
    echo "-- after request 700x500:"
    xdotool getwindowgeometry --window "$id" | grep -i 'geometry'
    xdotool search --onlyvisible --name bzdium | while read -r w; do xdotool windowkill "$w" 2>/dev/null; done
    pkill -f "$1/bzdium" 2>/dev/null
    sleep 2
}

run_case "$OLD_DIR" old
run_case "$NEW_DIR" new
echo "== logs: /tmp/ab-old.log /tmp/ab-new.log"
