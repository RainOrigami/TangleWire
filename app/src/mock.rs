//! A made-up graph in place of PipeWire, read from a scenario file. For
//! screenshots, demos and trying things out without touching real routing:
//! links made and removed here exist only in this process.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::connect::{self, End};
use crate::graph::{Direction, Event, Graph, Id, Link, Node, Port, Props};
use crate::pw::Command;

#[derive(Deserialize)]
struct Scenario {
    #[serde(default, rename = "device")]
    devices: Vec<DeviceDef>,
    #[serde(default, rename = "node")]
    nodes: Vec<NodeDef>,
    #[serde(default, rename = "link")]
    links: Vec<LinkDef>,
}

#[derive(Deserialize)]
struct DeviceDef {
    key: String,
    name: String,
    vendor: Option<String>,
}

#[derive(Deserialize)]
struct NodeDef {
    /// What links refer to; the name if left out.
    key: Option<String>,
    name: String,
    /// `media.class`, e.g. `Audio/Sink`. Left out for a node of the kind
    /// that is hidden unless asked for.
    class: Option<String>,
    app: Option<String>,
    /// `media.name`: what the stream carries.
    media: Option<String>,
    /// `key` of a device.
    device: Option<String>,
    link_group: Option<String>,
    /// `patchbay.group`.
    group: Option<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
    /// Give every input a monitor output, as sinks have.
    #[serde(default)]
    monitor: bool,
    /// `midi` or `video`; audio if left out.
    kind: Option<String>,
    /// Not there at first: a demo script makes the node appear.
    #[serde(default)]
    later: bool,
}

/// `from` and `to` are `node` or `node:port`. Whole nodes are linked port by
/// port, the way dragging one title onto another does.
#[derive(Deserialize)]
struct LinkDef {
    from: String,
    to: String,
}

pub struct Mock {
    /// Everything the scenario describes.
    graph: Graph,
    /// Nodes by the name links and scripts use for them.
    keys: HashMap<String, Id>,
    /// Nodes that have not appeared yet or have gone away again.
    absent: std::collections::HashSet<Id>,
    next: Id,
    emit: Box<dyn Fn(Event)>,
}

impl Mock {
    /// Reads the scenario and reports its graph through `emit`.
    pub fn load(path: &Path, emit: impl Fn(Event) + 'static) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let scenario: Scenario =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut mock = Self {
            graph: Graph::default(),
            keys: HashMap::new(),
            absent: Default::default(),
            next: 30,
            emit: Box::new(emit),
        };

        let mut devices = HashMap::new();
        for device in &scenario.devices {
            let id = mock.id();
            let mut props = Props::from([("device.description".to_owned(), device.name.clone())]);
            if let Some(vendor) = &device.vendor {
                props.insert("device.vendor.name".into(), vendor.clone());
            }
            devices.insert(device.key.as_str(), id);
            mock.graph.apply(Event::Device(id, props));
        }
        for def in &scenario.nodes {
            let id = mock.id();
            let key = def.key.as_deref().unwrap_or(&def.name);
            let mut props = Props::from([
                ("node.description".to_owned(), def.name.clone()),
                ("node.name".to_owned(), key.to_lowercase().replace(' ', "_")),
            ]);
            let optional = [
                ("media.class", &def.class),
                ("application.name", &def.app),
                ("media.name", &def.media),
                ("node.link-group", &def.link_group),
                ("patchbay.group", &def.group),
            ];
            for (name, value) in optional {
                if let Some(value) = value {
                    props.insert(name.to_owned(), value.clone());
                }
            }
            if let Some(device) = &def.device {
                let device = devices
                    .get(device.as_str())
                    .ok_or(format!("no device {device:?}"))?;
                props.insert("device.id".into(), device.to_string());
            }
            mock.keys.insert(key.to_owned(), id);
            if def.later {
                mock.absent.insert(id);
            }
            mock.graph.apply(Event::Node(Node { id, props }));

            let format = match def.kind.as_deref() {
                Some("midi") => "8 bit raw midi",
                Some("video") => "32 bit float RGBA video",
                _ => "32 bit float mono audio",
            };
            let monitors = def.inputs.iter().filter(|_| def.monitor).map(|name| {
                let channel = name.rsplit('_').next().unwrap_or(name);
                (format!("monitor_{channel}"), Direction::Out, true)
            });
            let inputs = def
                .inputs
                .iter()
                .map(|name| (name.clone(), Direction::In, false));
            let outputs = def
                .outputs
                .iter()
                .map(|name| (name.clone(), Direction::Out, false));
            let mut index = 0;
            for (name, direction, monitor) in inputs.chain(outputs).chain(monitors) {
                // `playback_FL` is on channel FL; names without such an ending
                // have no channel.
                let ending = name.rsplit('_').next().unwrap_or("");
                let is_channel = !ending.is_empty()
                    && ending != name
                    && ending
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
                let channel = is_channel.then(|| ending.to_owned());
                let port = Port {
                    id: mock.id(),
                    node: id,
                    name,
                    direction,
                    index,
                    channel,
                    format: Some(format.to_owned()),
                    monitor,
                    props: Props::new(),
                };
                index += 1;
                mock.graph.apply(Event::Port(port));
            }
        }
        for def in &scenario.links {
            let end = |text: &str| mock.resolve(text).ok_or(format!("nothing called {text:?}"));
            let plan = connect::plan(&mock.graph, &end(&def.from)?, &end(&def.to)?);
            if plan.is_empty() {
                return Err(format!("{} and {} cannot be linked", def.from, def.to));
            }
            for command in plan {
                if let Command::Link {
                    out_node,
                    out_port,
                    in_node,
                    in_port,
                } = command
                {
                    let id = mock.id();
                    mock.graph.apply(Event::Link(Link {
                        id,
                        out_node,
                        out_port,
                        in_node,
                        in_port,
                    }));
                }
            }
        }

        // Report what is there from the start.
        for (id, props) in mock.graph.devices.clone() {
            (mock.emit)(Event::Device(id, props));
        }
        let present: Vec<Id> = mock
            .graph
            .nodes
            .keys()
            .filter(|id| !mock.absent.contains(id))
            .copied()
            .collect();
        for id in present {
            mock.report(id);
        }
        (mock.emit)(Event::Synced);
        Ok(mock)
    }

    /// The node or port that `node` or `node:port` names.
    pub fn resolve(&self, text: &str) -> Option<End> {
        let (node, port) = match text.split_once(':') {
            Some((node, port)) => (node, Some(port)),
            None => (text, None),
        };
        let id = *self.keys.get(node)?;
        let Some(port) = port else {
            return Some(End::Node(id));
        };
        let found = self
            .graph
            .ports
            .values()
            .find(|p| p.node == id && p.name == port);
        found.map(|p| End::Ports(vec![p.id]))
    }

    /// Reports a node, its ports and its links to nodes that are there.
    fn report(&self, node: Id) {
        (self.emit)(Event::Node(self.graph.nodes[&node].clone()));
        for port in self.graph.ports.values().filter(|p| p.node == node) {
            (self.emit)(Event::Port(port.clone()));
        }
        let there = |id: Id| !self.absent.contains(&id);
        for link in self.graph.links.values() {
            // Each link once: with the later of its two nodes.
            let both = there(link.out_node) && there(link.in_node);
            if both && (link.out_node == node || link.in_node == node) {
                (self.emit)(Event::Link(link.clone()));
            }
        }
    }

    /// Makes a node of the scenario appear, as when an application starts.
    pub fn appear(&mut self, key: &str) {
        let Some(&id) = self.keys.get(key) else {
            return;
        };
        if self.absent.remove(&id) {
            self.report(id);
            (self.emit)(Event::Synced);
        }
    }

    /// Makes a node go away, as when an application stops playing.
    pub fn vanish(&mut self, key: &str) {
        let Some(&id) = self.keys.get(key) else {
            return;
        };
        if !self.absent.insert(id) {
            return;
        }
        let links = self
            .graph
            .links
            .values()
            .filter(|l| l.out_node == id || l.in_node == id);
        let ports = self.graph.ports.values().filter(|p| p.node == id);
        let gone: Vec<Id> = links
            .map(|l| l.id)
            .chain(ports.map(|p| p.id))
            .chain([id])
            .collect();
        for id in gone {
            (self.emit)(Event::Removed(id));
        }
        (self.emit)(Event::Synced);
    }

    fn id(&mut self) -> Id {
        self.next += 1;
        self.next
    }

    fn apply(&mut self, event: Event) {
        self.graph.apply(event.clone());
        (self.emit)(event);
    }

    fn change(&mut self, command: Command) {
        match command {
            Command::Link {
                out_node,
                out_port,
                in_node,
                in_port,
            } => {
                let id = self.id();
                self.apply(Event::Link(Link {
                    id,
                    out_node,
                    out_port,
                    in_node,
                    in_port,
                }));
            }
            Command::Unlink(id) => self.apply(Event::Removed(id)),
        }
    }

    /// Does what PipeWire would do with `command`.
    pub fn run(&mut self, command: Command) {
        self.change(command);
        (self.emit)(Event::Synced);
    }
}
