//! The PipeWire graph as the application sees it.

use std::collections::BTreeMap;

pub type Id = u32;
pub type Props = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: Id,
    pub props: Props,
}

impl Node {
    pub fn prop(&self, key: &str) -> Option<&str> {
        self.props.get(key).map(String::as_str)
    }

    /// Properties of the object `key` refers to, e.g. `device.id`.
    fn linked<'a>(&self, key: &str, objects: &'a BTreeMap<Id, Props>) -> Option<&'a Props> {
        objects.get(&self.prop(key)?.parse().ok()?)
    }

    pub fn device<'a>(&self, graph: &'a Graph) -> Option<&'a Props> {
        self.linked("device.id", &graph.devices)
    }

    pub fn client<'a>(&self, graph: &'a Graph) -> Option<&'a Props> {
        self.linked("client.id", &graph.clients)
    }

    pub fn name(&self) -> &str {
        self.prop("node.description")
            .or_else(|| self.prop("node.nick"))
            .or_else(|| self.prop("node.name"))
            .unwrap_or("?")
    }
}

#[derive(Debug, Clone)]
pub struct Port {
    pub id: Id,
    pub node: Id,
    pub name: String,
    pub direction: Direction,
    /// `port.id`: position among the node's ports of the same direction.
    pub index: u32,
    /// `audio.channel`, e.g. `FL`.
    pub channel: Option<String>,
    /// `format.dsp`; tells audio from MIDI from video.
    pub format: Option<String>,
    pub monitor: bool,
    /// Everything PipeWire says about the port.
    pub props: Props,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub id: Id,
    pub out_node: Id,
    pub out_port: Id,
    pub in_node: Id,
    pub in_port: Id,
}

/// A change to the graph, sent from the PipeWire thread.
#[derive(Debug, Clone)]
pub enum Event {
    /// A node appeared or its properties changed.
    Node(Node),
    Port(Port),
    Link(Link),
    /// A device and its properties.
    Device(Id, Props),
    /// A client (the connection of one application) and its properties.
    Client(Id, Props),
    Removed(Id),
    /// Everything that existed when the last change was made has been reported.
    Synced,
    /// Something went wrong that the connection survives.
    Notice(String),
    /// The connection to PipeWire failed or was lost.
    Error(String),
    /// A new connection is being made: forget everything reported so far.
    Reset,
}

#[derive(Debug, Default, Clone)]
pub struct Graph {
    pub nodes: BTreeMap<Id, Node>,
    pub ports: BTreeMap<Id, Port>,
    pub links: BTreeMap<Id, Link>,
    /// Properties of devices, by `device.id`.
    pub devices: BTreeMap<Id, Props>,
    /// Properties of clients, by `client.id`.
    pub clients: BTreeMap<Id, Props>,
}

impl Graph {
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Node(node) => {
                self.nodes.insert(node.id, node);
            }
            Event::Port(port) => {
                self.ports.insert(port.id, port);
            }
            Event::Link(link) => {
                self.links.insert(link.id, link);
            }
            Event::Device(id, props) => {
                self.devices.insert(id, props);
            }
            Event::Client(id, props) => {
                self.clients.insert(id, props);
            }
            Event::Removed(id) => {
                self.devices.remove(&id);
                self.clients.remove(&id);
                self.nodes.remove(&id);
                self.ports.remove(&id);
                self.links.remove(&id);
            }
            Event::Reset => *self = Self::default(),
            Event::Synced | Event::Notice(_) | Event::Error(_) => {}
        }
    }

    /// A name for a node that no other node has. Nodes that share a name
    /// (the inputs of one application, say) are told apart by what they
    /// play or record if that differs, else by their id.
    pub fn label(&self, id: Id) -> String {
        let Some(node) = self.nodes.get(&id) else {
            return "?".to_owned();
        };
        let twins: Vec<&Node> = self
            .nodes
            .values()
            .filter(|n| n.name() == node.name())
            .collect();
        let media = node.prop("media.name");
        let distinct = twins
            .iter()
            .filter(|n| n.prop("media.name") == media)
            .count()
            == 1;
        match (twins.len(), media) {
            (1, _) => node.name().to_owned(),
            (_, Some(media)) if distinct => format!("{} ({media})", node.name()),
            _ => format!("{} #{id}", node.name()),
        }
    }

    /// Ports of one side of a node, in the node's own order.
    pub fn ports_of(&self, node: Id, direction: Direction) -> Vec<&Port> {
        let mut ports: Vec<&Port> = self
            .ports
            .values()
            .filter(|p| p.node == node && p.direction == direction)
            .collect();
        ports.sort_by_key(|p| (p.index, p.id));
        ports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twins_get_names_of_their_own() {
        let mut graph = Graph::default();
        let node = |id, name: &str, media: Option<&str>| {
            let mut props = Props::from([("node.description".to_owned(), name.to_owned())]);
            if let Some(media) = media {
                props.insert("media.name".into(), media.into());
            }
            Event::Node(Node { id, props })
        };
        for event in [
            node(1, "Solo", Some("Song")),
            node(2, "OBS", Some("Desktop Audio")),
            node(3, "OBS", Some("Microphone")),
            node(4, "obs", None),
            node(5, "obs", None),
            node(6, "Player", Some("Same")),
            node(7, "Player", Some("Same")),
        ] {
            graph.apply(event);
        }
        assert_eq!(graph.label(1), "Solo");
        assert_eq!(graph.label(2), "OBS (Desktop Audio)");
        assert_eq!(graph.label(3), "OBS (Microphone)");
        assert_eq!(graph.label(4), "obs #4");
        assert_eq!(graph.label(7), "Player #7");
        assert_eq!(graph.label(99), "?");
    }
}
