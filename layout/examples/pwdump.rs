//! Lays out a saved `pw-dump` and writes the result as SVG.
//!
//! `pw-dump > graph.json && cargo run -p tanglewire-layout --example pwdump -- graph.json graph.svg`

use std::collections::BTreeMap;

use serde_json::Value;
use tanglewire_layout::{Engine, Graph, Layered, Link, Node, Port, Size, svg};

const HEADER: f64 = 26.0;
const ROW: f64 = 16.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: pwdump <pw-dump.json> <out.svg>");
        std::process::exit(2);
    };
    let dump: Value = serde_json::from_str(&std::fs::read_to_string(input).unwrap()).unwrap();
    let objects = dump.as_array().unwrap();
    let kind = |o: &Value, name: &str| o["type"] == format!("PipeWire:Interface:{name}");
    let id = |v: &Value| v.as_u64().unwrap() as u32;

    let mut nodes = BTreeMap::new();
    for o in objects.iter().filter(|o| kind(o, "Node")) {
        let props = &o["info"]["props"];
        // Nodes without a media class are internals (drivers, EasyEffects plumbing).
        if props["media.class"].is_null() {
            continue;
        }
        let label = ["node.description", "node.nick", "node.name"]
            .iter()
            .find_map(|key| props[key].as_str())
            .unwrap_or("?");
        nodes.insert(
            id(&o["id"]),
            Node {
                id: id(&o["id"]),
                label: label.into(),
                ..Node::default()
            },
        );
    }
    for o in objects.iter().filter(|o| kind(o, "Port")) {
        let props = &o["info"]["props"];
        let Some(node) = nodes.get_mut(&id(&props["node.id"])) else {
            continue;
        };
        let list = if o["info"]["direction"] == "input" {
            &mut node.inputs
        } else {
            &mut node.outputs
        };
        list.push(Port {
            id: id(&o["id"]),
            y: HEADER + ROW * (list.len() as f64 + 0.5),
            label: props["port.name"].as_str().unwrap_or("?").into(),
        });
    }
    for node in nodes.values_mut() {
        let rows = node.inputs.len().max(node.outputs.len());
        let widest = |ports: &[Port]| ports.iter().map(|p| p.label.len()).max().unwrap_or(0);
        let ports = (widest(&node.inputs) + widest(&node.outputs)) as f64 * 5.5 + 40.0;
        node.size = Size {
            w: ports.max(node.label.len() as f64 * 6.8 + 20.0).max(120.0),
            h: HEADER + ROW * rows as f64 + 6.0,
        };
    }
    let links = objects
        .iter()
        .filter(|o| kind(o, "Link"))
        .map(|o| Link {
            id: id(&o["id"]),
            from: id(&o["info"]["output-port-id"]),
            to: id(&o["info"]["input-port-id"]),
        })
        .collect();

    let graph = Graph {
        groups: Vec::new(),
        nodes: nodes.into_values().collect(),
        links,
    };
    let layout = Layered::default().layout(&graph);
    std::fs::write(&output, svg::render(&graph, &layout)).unwrap();
    println!(
        "{} nodes, {} links routed of {}, {:.0}x{:.0} -> {output}",
        graph.nodes.len(),
        layout.links.len(),
        graph.links.len(),
        layout.size.w,
        layout.size.h
    );
}
