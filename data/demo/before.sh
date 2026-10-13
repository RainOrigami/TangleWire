#!/bin/sh
# Makes docs/before.png: Helvum showing the graph of this machine, crossed
# out, and docs/after.png: the same graph in TangleWire. Needs Hyprland,
# helvum, grim and ImageMagick. Unlike the other pictures these two show the
# real PipeWire graph, names and all.
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
bin=${TANGLEWIRE:-$root/target/release/tanglewire}
[ -x "$bin" ] || bin=$root/target/debug/tanglewire
. data/demo/stage.sh
tanglewire_class=$STAGE_CLASS
STAGE_CLASS=org.pipewire.Helvum
mkdir -p docs
shot=$(mktemp --suffix=.png)

# Helvum puts its nodes in a few columns on the left, however wide the
# window is, so the window is made just wide enough for them.
width=${1:-1320} height=${2:-1400}

stage_show
address=$(stage_run helvum)
stage_size "$address" "$width" "$height"
sleep 3
area=$(hyprctl clients -j | jq -r --arg a "$address" '.[] | select(.address == $a) | "\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"')
grim -g "$area" "$shot"
pid=$(hyprctl clients -j | jq -r --arg a "$address" '.[] | select(.address == $a) | .pid')
kill "$pid"
stage_wait

STAGE_CLASS=$tanglewire_class
address=$(stage_run "$bin" --size 2560x1300 --screenshot "$root/docs/after.png")
stage_size "$address" 2560 1300
stage_wait
stage_hide
stage_trim docs/after.png

# A red cross over the whole picture and the question across the middle.
size=$(magick identify -format '%w %h' "$shot")
w=${size% *} h=${size#* }
magick "$shot" \
    -stroke '#e01b24' -strokewidth $((h / 22)) \
    -draw "line $((w / 8)),$((h / 8)) $((w * 7 / 8)),$((h * 7 / 8))" \
    -draw "line $((w * 7 / 8)),$((h / 8)) $((w / 8)),$((h * 7 / 8))" \
    -stroke none -fill '#000000c8' -draw "rectangle 0,$((h / 2 - w / 20)) $w,$((h / 2 + w / 20))" \
    -fill white -font DejaVu-Sans-Bold -pointsize $((w / 27)) -gravity center \
    -annotate +0+0 'Tired of unreadable PipeWire graphs?' \
    docs/before.png
rm -f "$shot"
echo "docs/before.png docs/after.png"
