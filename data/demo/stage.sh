# Shared by shots.sh and record.sh: where the demo window is shown. Under
# Hyprland it opens on a workspace of its own (TANGLEWIRE_WORKSPACE, 9 if
# unset). A window on a hidden workspace is never drawn, so that workspace is
# brought to the front for as long as pictures are taken, and the workspace
# shown before comes back afterwards.

STAGE_CLASS=net.codefreak.TangleWire.Demo
STAGE_WORKSPACE=${TANGLEWIRE_WORKSPACE:-9}

have_hyprland() { command -v hyprctl >/dev/null && hyprctl version >/dev/null 2>&1; }

# Hyprland with a Lua configuration takes dispatchers in Lua form, older
# versions in the classic one. $1 is the Lua form, the rest the classic.
dispatch() {
    lua=$1
    shift
    hyprctl dispatch "$lua" >/dev/null 2>&1 || hyprctl dispatch "$@" >/dev/null
}

# Starts the demo with the given arguments and prints its window's address
# (nothing without Hyprland, where it simply runs in the background).
stage_run() {
    if ! have_hyprland; then
        "$@" &
        return
    fi
    dispatch "hl.dsp.exec_cmd(\"[workspace $STAGE_WORKSPACE silent] $*\")" exec "[workspace $STAGE_WORKSPACE silent] $*"
    for _ in $(seq 50); do
        address=$(hyprctl clients -j | jq -r --arg c "$STAGE_CLASS" '.[] | select(.class == $c) | .address' | head -n 1)
        [ -n "$address" ] && break
        sleep 0.1
    done
    echo "$address"
}

# Waits until the demo has quit.
stage_wait() {
    if ! have_hyprland; then
        wait
        return
    fi
    while hyprctl clients -j | jq -e --arg c "$STAGE_CLASS" 'any(.[]; .class == $c)' >/dev/null; do
        sleep 0.2
    done
}

# Brings the demo's workspace to the front, and on exit puts every monitor
# back on the workspace it showed before, with the focus where it was.
stage_show() {
    have_hyprland || return 0
    STAGE_BEFORE=$(hyprctl monitors -j | jq -r '.[] | "\(.name) \(.activeWorkspace.id)"')
    STAGE_FOCUS=$(hyprctl monitors -j | jq -r '.[] | select(.focused) | .activeWorkspace.id')
    trap stage_hide EXIT INT TERM
    dispatch "hl.dsp.focus({ workspace = $STAGE_WORKSPACE })" workspace "$STAGE_WORKSPACE"
}

stage_hide() {
    trap - EXIT INT TERM
    have_hyprland || return 0
    echo "$STAGE_BEFORE" | while read -r monitor workspace; do
        now=$(hyprctl monitors -j | jq -r --arg n "$monitor" '.[] | select(.name == $n) | .activeWorkspace.id')
        [ "$now" = "$workspace" ] || dispatch "hl.dsp.focus({ workspace = $workspace })" workspace "$workspace"
    done
    dispatch "hl.dsp.focus({ workspace = $STAGE_FOCUS })" workspace "$STAGE_FOCUS"
}

# A graph is usually much wider than high. Cuts away the empty bands around
# it in the picture $1, leaving a margin in the background's colour.
stage_trim() {
    command -v magick >/dev/null || return 0
    background=$(magick "$1" -format '%[pixel:p{4,4}]' info:)
    magick "$1" -fuzz 3% -trim +repage -bordercolor "$background" -border 90 "$1"
}

# Floats the window at `address` with exactly this size.
stage_size() {
    [ -n "$1" ] || return 0
    window="window = \"address:$1\""
    dispatch "hl.dsp.window.float({ action = \"enable\", $window })" setfloating "address:$1"
    dispatch "hl.dsp.window.resize({ x = $2, y = $3, exact = true, $window })" resizewindowpixel "exact $2 $3,address:$1"
}
