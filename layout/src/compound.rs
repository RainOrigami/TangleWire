//! Nested groups on top of a flat engine.
//!
//! Every group is laid out on its own, innermost first, and then takes part in
//! its parent's layout as a single box. A link that crosses a group's frame is
//! cut in two there: inside the group it runs to a boundary point on the
//! frame, and the parent sees that point as a port of the box.

use std::collections::HashMap;

use crate::layered::{Layered, Side, flat};
use crate::{Curve, Graph, Id, Layout, Link, Node, Path, Point, Port, Rect, Size};

/// One end of a link as seen from inside one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum End {
    /// The port itself, or the point where the frame of a child group exposes it.
    Port(Id),
    /// The point on this group's own frame that stands for the port.
    Boundary(Id),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Child {
    Node(usize),
    Group(usize),
}

/// A group's contents, laid out in the group's own coordinates.
#[derive(Default)]
struct Solved {
    size: Size,
    linked: Rect,
    nodes: Vec<(usize, Point)>,
    groups: Vec<(usize, Point)>,
    pieces: HashMap<(End, End), Path>,
    /// Ports reachable from outside: port, whether it is an output, height.
    exposed: Vec<(Id, bool, f64)>,
}

struct Tree<'a> {
    cfg: &'a Layered,
    graph: &'a Graph,
    /// Groups around every node, outermost first.
    chains: Vec<Vec<usize>>,
    children: Vec<Vec<usize>>,
    port_node: HashMap<Id, usize>,
    solved: Vec<Solved>,
}

pub(crate) fn layout(cfg: &Layered, graph: &Graph) -> Layout {
    let index: HashMap<Id, usize> = graph
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.id, i))
        .collect();
    // Parent of every group; unknown parents and loops end the chain.
    let mut parent: Vec<Option<usize>> = graph
        .groups
        .iter()
        .map(|g| g.parent.and_then(|id| index.get(&id).copied()))
        .collect();
    for g in 0..parent.len() {
        let mut at = parent[g];
        for _ in 0..parent.len() {
            match at {
                Some(a) if a == g => {
                    parent[g] = None;
                    break;
                }
                Some(a) => at = parent[a],
                None => break,
            }
        }
    }
    let chain = |mut at: Option<usize>| {
        let mut chain = Vec::new();
        while let Some(g) = at {
            chain.push(g);
            at = parent[g];
        }
        chain.reverse();
        chain
    };
    let mut children = vec![Vec::new(); parent.len()];
    for (g, p) in parent.iter().enumerate() {
        if let Some(p) = *p {
            children[p].push(g);
        }
    }
    let mut port_node = HashMap::new();
    for (n, node) in graph.nodes.iter().enumerate() {
        for port in node.inputs.iter().chain(&node.outputs) {
            port_node.insert(port.id, n);
        }
    }
    let mut tree = Tree {
        cfg,
        graph,
        chains: graph
            .nodes
            .iter()
            .map(|n| chain(n.group.and_then(|id| index.get(&id).copied())))
            .collect(),
        children,
        port_node,
        solved: (0..parent.len()).map(|_| Solved::default()).collect(),
    };

    // Innermost groups first.
    let mut order: Vec<usize> = (0..parent.len()).collect();
    order.sort_by_key(|&g| std::cmp::Reverse(chain(Some(g)).len()));

    // A group is laid out without knowing where its outside links come from,
    // so two groups linked one-to-one may list their members in different
    // orders. Later rounds know from the round before where every port ended
    // up, and arrange each group's inputs in the order of their sources.
    // Changes travel one group downstream per round.
    let mut heights: Option<HashMap<Id, f64>> = None;
    for round in 0.. {
        for &g in &order {
            tree.solved[g] = tree.solve(Some(g), heights.as_ref());
        }
        let root = tree.solve(None, None);
        let mut out = Layout {
            size: root.size,
            linked: root.linked,
            ..Layout::default()
        };
        let mut origins = vec![Point::default(); parent.len()];
        tree.place(&root, Point::default(), &mut origins, &mut out);
        if round == ROUNDS || graph.groups.is_empty() {
            for link in &graph.links {
                if let Some(path) = tree.route(link, &root, &origins) {
                    out.links.insert(link.id, path);
                }
            }
            return out;
        }
        let ports = graph.nodes.iter().flat_map(|n| {
            let y = out.nodes[&n.id].y;
            n.inputs
                .iter()
                .chain(&n.outputs)
                .map(move |p| (p.id, y + p.y))
        });
        heights = Some(ports.collect());
    }
    unreachable!()
}

/// How often groups are laid out again to line up with their sources.
const ROUNDS: usize = 3;

impl Tree<'_> {
    /// Where a group's contents start, relative to its frame.
    fn inset(&self, g: usize) -> Point {
        Point {
            x: self.cfg.group_pad,
            y: self.graph.groups[g].header.h + self.cfg.group_pad,
        }
    }

    fn frame(&self, g: usize) -> Size {
        let pad = self.cfg.group_pad;
        let (inner, header) = (self.solved[g].size, self.graph.groups[g].header);
        Size {
            w: inner.w.max(header.w) + 2.0 * pad,
            h: inner.h + header.h + 2.0 * pad,
        }
    }

    /// The direct child of `scope` that holds node `n`, if `n` is inside `scope`.
    fn child(&self, scope: Option<usize>, n: usize) -> Option<Child> {
        let chain = &self.chains[n];
        let depth = match scope {
            Some(g) => chain.iter().position(|&c| c == g)? + 1,
            None => 0,
        };
        Some(
            chain
                .get(depth)
                .map_or(Child::Node(n), |&g| Child::Group(g)),
        )
    }

    /// Lays out the contents of `scope`. `heights` is where every port was in
    /// the previous round.
    fn solve(&self, scope: Option<usize>, heights: Option<&HashMap<Id, f64>>) -> Solved {
        let graph = self.graph;
        let mut next = 0;
        let mut fresh = || {
            next += 1;
            next
        };
        let mut local: HashMap<End, Id> = HashMap::new();
        let mut sub = Graph::default();
        let mut solved = Solved::default();

        let mut members = Vec::new();
        for (n, node) in graph.nodes.iter().enumerate() {
            if self.child(scope, n) != Some(Child::Node(n)) {
                continue;
            }
            let mut port = |p: &Port| {
                let id = fresh();
                local.insert(End::Port(p.id), id);
                Port {
                    id,
                    y: p.y,
                    label: String::new(),
                }
            };
            let inputs = node.inputs.iter().map(&mut port).collect();
            let outputs = node.outputs.iter().map(&mut port).collect();
            let id = fresh();
            members.push((id, Child::Node(n)));
            sub.nodes.push(Node {
                id,
                before: node.before,
                size: node.size,
                inputs,
                outputs,
                ..Node::default()
            });
        }
        let groups = match scope {
            Some(g) => self.children[g].clone(),
            None => (0..graph.groups.len())
                .filter(|&g| self.chains_root(g))
                .collect(),
        };
        for g in groups {
            let inset = self.inset(g);
            // A group was where its topmost member was.
            let inside = |n: &usize| self.chains[*n].contains(&g);
            let inner = (0..graph.nodes.len())
                .filter(inside)
                .filter_map(|n| graph.nodes[n].before);
            let before = inner.min_by(|a, b| a.y.partial_cmp(&b.y).unwrap());
            let mut node = Node {
                id: fresh(),
                before,
                size: self.frame(g),
                ..Node::default()
            };
            for &(port, output, y) in &self.solved[g].exposed {
                let id = fresh();
                local.insert(End::Port(port), id);
                let list = if output {
                    &mut node.outputs
                } else {
                    &mut node.inputs
                };
                list.push(Port {
                    id,
                    y: inset.y + y,
                    label: String::new(),
                });
            }
            members.push((node.id, Child::Group(g)));
            sub.nodes.push(node);
        }

        // Links, and the boundary points needed by those that leave the group.
        let mut keys = Vec::new();
        let mut boundary: Vec<(Child, Side, Id)> = Vec::new();
        for link in &graph.links {
            let (Some(&a), Some(&b)) =
                (self.port_node.get(&link.from), self.port_node.get(&link.to))
            else {
                continue;
            };
            let key = match (self.child(scope, a), self.child(scope, b)) {
                (Some(ca), Some(cb)) if ca != cb || matches!(ca, Child::Node(_)) => {
                    (End::Port(link.from), End::Port(link.to))
                }
                (Some(ca), None) => {
                    boundary.push((ca, Side::East, link.from));
                    (End::Port(link.from), End::Boundary(link.from))
                }
                (None, Some(cb)) => {
                    boundary.push((cb, Side::West, link.to));
                    (End::Boundary(link.to), End::Port(link.to))
                }
                _ => continue,
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        // One boundary node per child and side, its ports in the child's order.
        let height = |port: Id| {
            let id = local[&End::Port(port)];
            let ports = sub
                .nodes
                .iter()
                .flat_map(|n| n.inputs.iter().chain(&n.outputs));
            ports
                .filter(|p| p.id == id)
                .map(|p| p.y)
                .next()
                .unwrap_or(0.0)
        };
        boundary.sort_by(|a, b| height(a.2).partial_cmp(&height(b.2)).unwrap());
        boundary.dedup();
        let mut pins = HashMap::new();
        let mut order = HashMap::new();
        let mut walls: Vec<(Id, Side, Vec<Id>)> = Vec::new();
        for (child, side) in members
            .iter()
            .flat_map(|m| [(m.1, Side::West), (m.1, Side::East)])
        {
            let ports: Vec<Id> = boundary
                .iter()
                .filter(|b| b.0 == child && b.1 == side)
                .map(|b| b.2)
                .collect();
            if ports.is_empty() {
                continue;
            }
            let mut node = Node {
                id: fresh(),
                ..Node::default()
            };
            node.size.h = (ports.len() - 1) as f64 * self.cfg.link_gap;
            for (i, &port) in ports.iter().enumerate() {
                let id = fresh();
                local.insert(End::Boundary(port), id);
                let port = Port {
                    id,
                    y: i as f64 * self.cfg.link_gap,
                    label: String::new(),
                };
                match side {
                    Side::West => node.outputs.push(port),
                    Side::East => node.inputs.push(port),
                }
            }
            pins.insert(node.id, side);
            // Inputs line up with where their links come from.
            if let (Side::West, Some(heights)) = (side, heights) {
                let sources: Vec<f64> = graph
                    .links
                    .iter()
                    .filter(|l| ports.contains(&l.to))
                    .filter(|l| {
                        self.port_node
                            .get(&l.from)
                            .is_some_and(|&n| self.child(scope, n).is_none())
                    })
                    .filter_map(|l| heights.get(&l.from).copied())
                    .collect();
                if !sources.is_empty() {
                    order.insert(node.id, sources.iter().sum::<f64>() / sources.len() as f64);
                }
            }
            walls.push((node.id, side, ports));
            sub.nodes.push(node);
        }
        for (i, key) in keys.iter().enumerate() {
            sub.links.push(Link {
                id: i as Id,
                from: local[&key.0],
                to: local[&key.1],
            });
        }

        let result = flat(self.cfg, &sub, &pins, &order);
        solved.size = result.size;
        solved.linked = result.linked;
        for (id, child) in members {
            let at = result.nodes[&id];
            match child {
                Child::Node(n) => solved.nodes.push((n, at)),
                Child::Group(g) => solved.groups.push((g, at)),
            }
        }
        for (id, side, ports) in walls {
            let at = result.nodes[&id];
            for (i, port) in ports.into_iter().enumerate() {
                let y = at.y + i as f64 * self.cfg.link_gap;
                solved.exposed.push((port, side == Side::East, y));
            }
        }
        for (i, key) in keys.into_iter().enumerate() {
            if let Some(path) = result.links.get(&(i as Id)) {
                solved.pieces.insert(key, path.clone());
            }
        }
        solved
    }

    /// Whether group `g` sits directly in the root.
    fn chains_root(&self, g: usize) -> bool {
        !self.children.iter().any(|c| c.contains(&g))
    }

    /// Turns positions relative to a group into absolute ones, top down.
    fn place(&self, solved: &Solved, origin: Point, origins: &mut [Point], out: &mut Layout) {
        let at = |p: Point| Point {
            x: origin.x + p.x,
            y: origin.y + p.y,
        };
        for &(n, p) in &solved.nodes {
            out.nodes.insert(self.graph.nodes[n].id, at(p));
        }
        for &(g, p) in &solved.groups {
            let (min, size, inset) = (at(p), self.frame(g), self.inset(g));
            let max = Point {
                x: min.x + size.w,
                y: min.y + size.h,
            };
            out.groups
                .insert(self.graph.groups[g].id, Rect { min, max });
            origins[g] = Point {
                x: min.x + inset.x,
                y: min.y + inset.y,
            };
            self.place(&self.solved[g], origins[g], origins, out);
        }
    }

    /// Joins the pieces of a link from the innermost group of its source, out
    /// to the group both ends share, and in again to its target.
    fn route(&self, link: &Link, root: &Solved, origins: &[Point]) -> Option<Path> {
        let a = &self.chains[*self.port_node.get(&link.from)?];
        let b = &self.chains[*self.port_node.get(&link.to)?];
        let shared = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        let (from, to) = (End::Port(link.from), End::Port(link.to));
        let mut stops = Vec::new();
        for &g in a[shared..].iter().rev() {
            stops.push((Some(g), (from, End::Boundary(link.from))));
        }
        stops.push((shared.checked_sub(1).map(|i| a[i]), (from, to)));
        for &g in &b[shared..] {
            stops.push((Some(g), (End::Boundary(link.to), to)));
        }

        let mut path: Option<Path> = None;
        for (scope, key) in stops {
            let (solved, origin) = match scope {
                Some(g) => (&self.solved[g], origins[g]),
                None => (root, Point::default()),
            };
            let piece = solved.pieces.get(&key)?;
            let at = |p: Point| Point {
                x: origin.x + p.x,
                y: origin.y + p.y,
            };
            let start = at(piece.start);
            let path = path.get_or_insert(Path {
                start,
                curves: Vec::new(),
            });
            let end = path.curves.last().map_or(path.start, |c| c.end);
            if end != start {
                // Straight across the padding between a frame and its contents.
                let mix = |t: f64| Point {
                    x: end.x + (start.x - end.x) * t,
                    y: end.y + (start.y - end.y) * t,
                };
                path.curves.push(Curve {
                    c1: mix(1.0 / 3.0),
                    c2: mix(2.0 / 3.0),
                    end: start,
                });
            }
            path.curves.extend(piece.curves.iter().map(|c| Curve {
                c1: at(c.c1),
                c2: at(c.c2),
                end: at(c.end),
            }));
        }
        path
    }
}
