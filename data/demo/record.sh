#!/bin/sh
# Records the demo tour into docs/tour.mp4 and docs/tour.gif. The application
# films its own window (ffmpeg is needed), so neither the mouse pointer nor
# anything else on the screen is in the picture. Under Hyprland the window
# opens on a workspace of its own, which is shown meanwhile (see stage.sh).
set -eu
cd "$(dirname "$0")/../.."
root=$PWD
bin=${TANGLEWIRE:-$root/target/release/tanglewire}
[ -x "$bin" ] || bin=$root/target/debug/tanglewire
# With a size, e.g. `record.sh 1920 1080`, the window floats at that size;
# without, it fills the screen.
width=${1:-} height=${2:-}
. data/demo/stage.sh
mkdir -p docs
rm -f docs/tour.mp4

stage_show
address=$(stage_run "$bin" --mock "$root/data/demo/streamer.toml" --demo "$root/data/demo/tour.toml" \
    --record "$root/docs/tour.mp4")
if [ -n "$width" ]; then
    stage_size "$address" "$width" "$height"
elif [ -n "$address" ]; then
    dispatch "hl.dsp.window.fullscreen({ mode = \"fullscreen\", window = \"address:$address\" })" \
        fullscreen "0,address:$address"
fi
stage_wait
stage_hide

ffmpeg -y -loglevel error -i docs/tour.mp4 \
    -vf "fps=12,scale=1280:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer" \
    docs/tour.gif
echo "docs/tour.mp4 docs/tour.gif"
