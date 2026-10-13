//! Graph layout for TangleWire.
//!
//! Pure geometry: takes nodes with sizes, ports and links, and returns node
//! positions and link paths. Arrangement methods implement [`Engine`] so the
//! application can switch between them at runtime.

use std::collections::HashMap;

mod compound;
mod layered;
pub mod svg;

pub use layered::Layered;

pub type Id = u32;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub min: Point,
    pub max: Point,
}

impl Rect {
    pub fn size(&self) -> Size {
        Size {
            w: self.max.x - self.min.x,
            h: self.max.y - self.min.y,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub groups: Vec<Group>,
}

/// A container drawn around its member nodes and groups.
#[derive(Debug, Clone, Default)]
pub struct Group {
    pub id: Id,
    pub parent: Option<Id>,
    /// Space reserved for the title along the top edge.
    pub header: Size,
}

/// A box with input ports on its left edge and output ports on its right edge.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub id: Id,
    pub size: Size,
    /// Innermost group the node belongs to.
    pub group: Option<Id>,
    /// Where the node was in the layout shown so far. The engine starts from
    /// that arrangement, so that a small change to the graph does not
    /// reshuffle everything.
    pub before: Option<Point>,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    /// Only used by the SVG dump.
    pub label: String,
}

#[derive(Debug, Clone, Default)]
pub struct Port {
    /// Unique across the whole graph.
    pub id: Id,
    /// Distance of the port's centre from the top of its node.
    pub y: f64,
    /// Only used by the SVG dump.
    pub label: String,
}

/// A connection from an output port to an input port.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    pub id: Id,
    pub from: Id,
    pub to: Id,
}

/// One cubic Bézier segment, continuing from the end of the previous one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    pub c1: Point,
    pub c2: Point,
    pub end: Point,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Path {
    pub start: Point,
    pub curves: Vec<Curve>,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Top-left corner of every node.
    pub nodes: HashMap<Id, Point>,
    /// Route of every link whose two ports exist.
    pub links: HashMap<Id, Path>,
    /// Frame of every group.
    pub groups: HashMap<Id, Rect>,
    pub size: Size,
    /// Bounds of the nodes that have links, and of those links. The whole
    /// layout if nothing is linked.
    pub linked: Rect,
}

/// An arrangement method. Implementations must be deterministic: the same
/// input yields the same output.
pub trait Engine {
    /// Stable identifier, used in the config file.
    fn id(&self) -> &'static str;
    /// Name shown to the user.
    fn name(&self) -> &'static str;
    fn layout(&self, graph: &Graph) -> Layout;
}

/// Every engine built into this crate, default first.
pub fn engines() -> Vec<Box<dyn Engine>> {
    vec![Box::new(Layered::default())]
}
