//! SVG rendering of a layout, for inspecting results without the GUI.

use std::fmt::Write;

use crate::{Graph, Layout};

const PALETTE: [&str; 8] = [
    "#62a0ea", "#57e389", "#f8e45c", "#ffa348", "#ed333b", "#c061cb", "#5bc8af", "#b5835a",
];

pub fn render(graph: &Graph, layout: &Layout) -> String {
    let pad = 24.0;
    let (w, h) = (layout.size.w + 2.0 * pad, layout.size.h + 2.0 * pad);
    let mut s = String::new();
    let _ = writeln!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {w} {h}" width="{w}" height="{h}" font-family="sans-serif">
<rect x="{}" y="{}" width="{w}" height="{h}" fill="#1e1e1e"/>"##,
        -pad, -pad, -pad, -pad
    );
    let mut colour = std::collections::HashMap::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        for port in &node.outputs {
            colour.insert(port.id, PALETTE[i % PALETTE.len()]);
        }
    }
    for link in &graph.links {
        let Some(path) = layout.links.get(&link.id) else {
            continue;
        };
        let mut d = format!("M{:.1} {:.1}", path.start.x, path.start.y);
        for c in &path.curves {
            let _ = write!(
                d,
                " C{:.1} {:.1} {:.1} {:.1} {:.1} {:.1}",
                c.c1.x, c.c1.y, c.c2.x, c.c2.y, c.end.x, c.end.y
            );
        }
        let stroke = colour.get(&link.from).copied().unwrap_or("#888");
        let _ = writeln!(
            s,
            r#"<path d="{d}" fill="none" stroke="{stroke}" stroke-width="1.6" opacity="0.9"/>"#
        );
    }
    for node in &graph.nodes {
        let Some(p) = layout.nodes.get(&node.id) else {
            continue;
        };
        let _ = writeln!(
            s,
            r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="6" fill="#303030" stroke="#5a5a5a"/>
<text x="{:.1}" y="{:.1}" font-size="11" font-weight="bold" fill="#eee">{}</text>"##,
            p.x,
            p.y,
            node.size.w,
            node.size.h,
            p.x + 8.0,
            p.y + 16.0,
            escape(&node.label)
        );
        for (ports, x, anchor, dx) in [
            (&node.inputs, p.x, "start", 7.0),
            (&node.outputs, p.x + node.size.w, "end", -7.0),
        ] {
            for port in ports {
                let y = p.y + port.y;
                let _ = writeln!(
                    s,
                    r##"<circle cx="{x:.1}" cy="{y:.1}" r="3.5" fill="#bbb"/>
<text x="{:.1}" y="{:.1}" font-size="9" fill="#bbb" text-anchor="{anchor}">{}</text>"##,
                    x + dx,
                    y + 3.0,
                    escape(&port.label)
                );
            }
        }
    }
    s.push_str("</svg>\n");
    s
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
