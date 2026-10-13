<p align="center">
  <img src="data/net.codefreak.TangleWire.svg" width="96" alt="">
</p>

<h1 align="center">TangleWire</h1>

<p align="center">A PipeWire patchbay that lays itself out.</p>

<p align="center">
  <a href="https://github.com/RainOrigami/tanglewire/actions/workflows/ci.yml"><img src="https://github.com/RainOrigami/tanglewire/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/RainOrigami/tanglewire/releases/latest"><img src="https://img.shields.io/github/v/release/RainOrigami/tanglewire" alt="Latest release"></a>
  <a href="UNLICENSE"><img src="https://img.shields.io/badge/license-Unlicense-blue" alt="License: Unlicense"></a>
  <img src="https://img.shields.io/badge/GTK-4-4a86cf" alt="GTK 4">
  <img src="https://img.shields.io/badge/PipeWire-native-57e389" alt="PipeWire">
</p>

TangleWire shows the PipeWire graph (audio, MIDI and video) and lets you
connect and disconnect ports, like Helvum or qpwgraph. The difference is that
you never arrange anything: nodes are placed in columns in the direction the
signal flows, links are routed through the gaps between them and never across
a node, and related nodes are grouped. When the graph changes, the layout
adjusts and shows you what changed.

<p align="center">
<!-- picture: before.png, width="760" --><img width="1320" height="1400" alt="before" src="https://github.com/user-attachments/assets/e3a8c5ab-7318-47d1-a96b-1d06ddf10ebb" />

</p>

<p align="center"><b>TangleWire sorts it out:</b></p>

<p align="center">
<!-- picture: after.png --><img width="2553" height="496" alt="after" src="https://github.com/user-attachments/assets/bc622e80-1285-4c6b-be79-8b19b419d707" />

</p>

<p align="center">The same graph, on the same machine.</p>

> [!WARNING]
> **This application was written by an AI.** All of the code was produced by
> Claude, an AI coding agent, under the direction of the maintainer, who
> decided what it should do and how it should behave and tested it by using
> it. Nobody has reviewed the code line by line.
>
> Be as careful with it as with any other software nobody has audited. It
> changes the links in your PipeWire graph when you tell it to, runs with
> your user's rights and has had one user so far. Expect bugs, read the code
> before relying on it, and try things in [demo mode](#demo-mode) first if
> your routing matters.

## Features

- **Automatic layout.** Sources on the left, sinks on the right, links routed
  around nodes. Stereo pairs run as parallel bundles. A new stream slots into
  the existing arrangement instead of reshuffling it.
- **Groups, nested.** The two halves of a loopback or filter chain, the nodes
  of one device and the streams of one application are framed together.
  Groups can be collapsed to a single card, and you can define your own.
  Inside a group, names drop what its title says already: in "Scarlett Solo
  4th Gen", "Scarlett Solo 4th Gen Headphones" is just "Headphones".
- **Linking by drag.** Port to port links them, the same drag again unlinks.
  Drag a node's title onto another node to link every channel (FL to FL, FR
  to FR). A tip says what the drop will do before you let go.
- **Changes stay visible.** New or changed nodes get an orange rim, with an
  arrow at the edge if they are out of view. Streams and links that went away
  remain as ghosts until the next change, so you can see what disconnected.
- **Hidden nodes leave no hole.** Internal nodes are hidden, and a dashed
  link joins what is connected through them. One key shows them, marked.
- **Split view.** One row per link instead of one per port, for sorting out
  busy nodes.
- **Search.** By name, port, id or any PipeWire property (application,
  process, device vendor, …). Tolerates typos, left-out letters and
  sound-alikes; `/…/` is a regular expression.
- **Node list.** A filterable list of every node and port to drag links from,
  so the source is always in the same place.
- **Find the other end.** Hover a port to see what it is linked to; right
  click to jump there and Backspace to come back.
- **No window chrome.** No title bar, toolbar or menus. Follows the system's
  dark or light theme.

## See it in action

<!-- video: tour.mp4, on a line of its own -->


https://github.com/user-attachments/assets/94f13080-ebec-48c4-8ad7-c99e524d1fbd



## More pictures

An ordinary desktop:

<!-- picture: desktop.png, width="760" --><img width="760" alt="desktop" src="https://github.com/user-attachments/assets/6cf7db39-db26-4514-bfb7-18d3b3662051" />


A streaming setup, with a sink for each kind of sound and a processed
microphone. The dashed links from Comms to Monitor stand for two hidden effect
nodes between them:

<!-- picture: streamer.png --><img width="5865" height="1085" alt="streamer" src="https://github.com/user-attachments/assets/a6acb52f-73b6-4fbf-b88e-dcf2a070d6e5" />


The same in split view, one row per link:

<!-- picture: streamer-split.png --><img width="5865" height="1083" alt="streamer-split" src="https://github.com/user-attachments/assets/a3a0c742-f3e4-49d4-95b4-cb58efa92fc6" />


A music studio, with two audio interfaces, a DAW, a plugin rack and MIDI gear:

<!-- picture: studio.png --><img width="7273" height="2091" alt="studio" src="https://github.com/user-attachments/assets/79020558-357f-4ad7-bbae-8d0ff93b7841" />


## Install

### Release builds

Every tagged version is built automatically and published on the
[releases page](https://github.com/RainOrigami/tanglewire/releases/latest).
The archive holds the `tanglewire` binary, the desktop file and the icon:

```sh
tar -xf tanglewire-v*-x86_64-linux.tar.gz
cd tanglewire-v*-x86_64-linux
install -Dm755 tanglewire ~/.local/bin/tanglewire
install -Dm644 net.codefreak.TangleWire.desktop ~/.local/share/applications/net.codefreak.TangleWire.desktop
install -Dm644 net.codefreak.TangleWire.svg ~/.local/share/icons/hicolor/scalable/apps/net.codefreak.TangleWire.svg
```

The builds are made on Debian 13 and need GTK 4.18, libadwaita 1.7 and glibc
2.41 or newer at run time. On older systems, build from source.

### From source

You need a Rust toolchain (1.92 or newer), clang, pkg-config and the
development files of GTK 4 (4.16 or newer), libadwaita (1.6 or newer) and
PipeWire.

| Distribution | Packages |
| --- | --- |
| Arch | `rust clang pkgconf gtk4 libadwaita pipewire` |
| Debian, Ubuntu | `build-essential clang pkg-config libgtk-4-dev libadwaita-1-dev libpipewire-0.3-dev` and Rust from [rustup](https://rustup.rs) |
| Fedora | `rust cargo clang pkgconf-pkg-config gtk4-devel libadwaita-devel pipewire-devel` |

```sh
cargo build --release
install -Dm755 target/release/tanglewire ~/.local/bin/tanglewire
install -Dm644 data/net.codefreak.TangleWire.desktop ~/.local/share/applications/net.codefreak.TangleWire.desktop
install -Dm644 data/net.codefreak.TangleWire.svg ~/.local/share/icons/hicolor/scalable/apps/net.codefreak.TangleWire.svg
```

## Using it

Press **F1** in the application for this list.

| Key | |
| --- | --- |
| `F` | Fit the linked part of the graph into view |
| `S` | Split view: one row per link |
| `L` | List of all nodes and ports, to drag links from |
| `G` | Collapse all groups, or expand them all |
| `H` | Show hidden nodes, marked as such, or hide them again |
| `Ctrl+F` or `/` | Search |
| `Enter`, `Ctrl+G` | Go to the next match (`Ctrl+Shift+G`: previous) |
| `Esc` | Close search or list |
| Arrow keys | Move the view |
| `Backspace` | Back to the view before a jump |
| `Delete` | Remove the selected link |
| `Ctrl+R` | Lay the graph out again |

| Mouse | |
| --- | --- |
| Drag the background | Move the view (middle or right drag works anywhere) |
| Scroll | Zoom |
| Drag port to port | Link them, or unlink if they are linked |
| Drag title to title | Link all ports, matched by channel |
| Drag title to port | Link the port that matches |
| `Ctrl`+click ports | Select several, then drag any one of them |
| Click a link | Select it |
| Hover a port | Show what it is linked to |
| Right click a port | Jump to the other end of its link |
| Click a group title | Collapse or expand the group |

Links you make stay when TangleWire closes. If a session manager rule or
another tool manages your routing, it may put links back or remove yours.

## Groups and hidden nodes

Which group a node belongs to is decided by the first of these that applies:

1. The node property `patchbay.group`, a slash-separated path such as
   `mic/fx`. Set it where the node is defined (filter chain, loopback, null
   sink).
2. A rule in `~/.config/tanglewire/config.toml`.
3. Automatically: nodes with the same `node.link-group`, nested inside the
   group of their device where they belong to one, then nodes of the same
   application or with the same name. An application that does not put its
   name on its nodes (Easy Effects) is known by the name it connected with.

Rules match node properties against patterns in which `*` stands for any
text. The first matching rule wins.

```toml
[[rule]]
match = { "node.name" = "mic_*", "media.class" = "Audio/*" }
group = "mic/fx"

[[rule]]
match = { "application.name" = "Steam Voice Settings" }
hide = true
```

Nodes without a `media.class` (drivers, internal plumbing) are hidden unless a
rule says `hide = false`.

`H` shows the hidden nodes, in the groups they belong to, with a dashed border
and `(hidden)` after their name, until it is pressed again. While they are
hidden, a long-dashed link joins what is connected through them, so that a
hidden effect chain between an application and a sink does not look like a
missing link. Such a link can be
selected and jumped along like any other, but not deleted: hover over one of
its ports to see which hidden nodes it stands for.

## Demo mode

`tanglewire --mock <scenario.toml>` shows a made-up graph instead of
PipeWire's. Links made there exist only in that window, so it is a safe place
to try things. Three scenarios are in [`data/demo`](data/demo): an ordinary
desktop, a streaming setup and a music studio.

```sh
tanglewire --mock data/demo/studio.toml
```

The pictures and the video on this page are made from those scenarios:
`data/demo/shots.sh` takes the pictures, and `data/demo/record.sh` plays
[`data/demo/tour.toml`](data/demo/tour.toml), a script that moves a drawn
pointer through the application, and has the application film itself.
`data/demo/before.sh` makes the crossed-out picture of Helvum and the one of
the same graph in TangleWire, both of the machine it runs on. All of them
write to `docs/`.

## How it works

The repository is a Cargo workspace of two crates.

- [`layout`](layout) knows nothing about PipeWire or GTK. It takes nodes with
  sizes and ports, groups and links, and returns positions and link routes.
  The engine is a layered (Sugiyama-style) layout with ports and nested
  groups. Engines implement one trait, so other arrangements can be added.
- [`app`](app) is the GTK 4 / libadwaita application. PipeWire runs on its own
  thread; nodes are ordinary widgets, links are drawn by the canvas.

## License

[Unlicense](UNLICENSE): public domain.
