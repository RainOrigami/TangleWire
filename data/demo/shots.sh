#!/bin/sh
# Takes the pictures of the demo scenarios, into docs/. Under Hyprland the
# window opens at a fixed size on a workspace of its own, which is shown
# meanwhile (see stage.sh); elsewhere it has whatever size the window manager
# gives it.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
bin=${TANGLEWIRE:-$root/target/release/tanglewire}
[ -x "$bin" ] || bin=$root/target/debug/tanglewire
. data/demo/stage.sh
mkdir -p docs

shot() { # name, scenario, width, height, further options
    name=$1 scenario=$2 width=$3 height=$4
    shift 4
    address=$(stage_run "$bin" --mock "$root/data/demo/$scenario.toml" --size "${width}x$height" \
        --screenshot "$root/docs/$name.png" "$@")
    stage_size "$address" "$width" "$height"
    stage_wait
    stage_trim "docs/$name.png"
    echo "docs/$name.png"
}

stage_show
shot desktop desktop 1400 800
shot streamer streamer 1920 1080
shot streamer-split streamer 1920 1080 --split
shot studio studio 2400 1300
stage_hide
