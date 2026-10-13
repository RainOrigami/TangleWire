//! Layered (Sugiyama-style) layout: nodes sit in columns with dataflow running
//! left to right, and links are routed through the gaps between columns.

use std::collections::{BTreeMap, HashMap};

use crate::{Curve, Engine, Graph, Id, Layout, Path, Point, Rect, Size};

#[derive(Debug, Clone)]
pub struct Layered {
    /// Minimum horizontal distance between two columns.
    pub layer_gap: f64,
    /// Vertical distance between two nodes in a column.
    pub node_gap: f64,
    /// Vertical distance between two parallel links.
    pub link_gap: f64,
    /// Distance between a group's frame and its contents.
    pub group_pad: f64,
}

impl Default for Layered {
    fn default() -> Self {
        Self {
            layer_gap: 90.0,
            node_gap: 22.0,
            link_gap: 9.0,
            group_pad: 12.0,
        }
    }
}

impl Engine for Layered {
    fn id(&self) -> &'static str {
        "layered"
    }

    fn name(&self) -> &'static str {
        "Layered"
    }

    fn layout(&self, graph: &Graph) -> Layout {
        crate::compound::layout(self, graph)
    }
}

/// Edge of the layout a node is pinned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    West,
    East,
}

/// Lays out `graph` ignoring its groups. Pinned nodes get the first or last
/// column to themselves. A column whose nodes all appear in `order` keeps
/// them sorted by that value.
pub(crate) fn flat(
    cfg: &Layered,
    graph: &Graph,
    pins: &HashMap<Id, Side>,
    order: &HashMap<Id, f64>,
) -> Layout {
    let mut state = State::new(cfg, graph);
    state.hints = graph
        .nodes
        .iter()
        .map(|n| order.get(&n.id).copied())
        .collect();
    state.pins = graph
        .nodes
        .iter()
        .map(|n| pins.get(&n.id).copied())
        .collect();
    state.break_cycles();
    state.assign_layers();
    state.build_verts();
    state.order();
    state.assign_y();
    state.finish()
}

struct PortRef {
    node: usize,
    output: bool,
    y: f64,
}

/// All links from one node to another. They are routed as a parallel bundle.
struct Edge {
    from: usize,
    to: usize,
    /// Indices into `Graph::links`, top to bottom.
    links: Vec<usize>,
    /// Points against the flow; set for one edge of every cycle.
    reversed: bool,
    /// Mean port offset at either end.
    from_y: f64,
    to_y: f64,
    /// Dummy vertices, by ascending layer.
    chain: Vec<usize>,
}

impl Edge {
    /// End points in layering order.
    fn dag(&self) -> (usize, usize) {
        if self.reversed {
            (self.to, self.from)
        } else {
            (self.from, self.to)
        }
    }
}

/// Something that occupies a slot in a column: a node, or the place where a
/// bundle of links passes through.
#[derive(Clone)]
struct Vert {
    node: Option<usize>,
    layer: usize,
    h: f64,
    y: f64,
    /// Vertex in the same column this one stays directly below.
    anchor: Option<usize>,
}

/// The part of an edge between two adjacent columns.
struct Seg {
    a: usize,
    b: usize,
    ya: f64,
    yb: f64,
    w: f64,
}

/// A neighbour in an adjacent column.
#[derive(Clone, Copy)]
struct Nb {
    vert: usize,
    y_self: f64,
    y_other: f64,
    w: f64,
}

struct State<'a> {
    cfg: &'a Layered,
    graph: &'a Graph,
    ports: HashMap<Id, PortRef>,
    edges: Vec<Edge>,
    self_links: Vec<usize>,
    pins: Vec<Option<Side>>,
    hints: Vec<Option<f64>>,
    /// Layer of every node that has an edge.
    layer: Vec<Option<usize>>,
    verts: Vec<Vert>,
    node_vert: Vec<usize>,
    segs: Vec<Seg>,
    left: Vec<Vec<Nb>>,
    right: Vec<Vec<Nb>>,
    /// Vertices per column, top to bottom.
    layers: Vec<Vec<usize>>,
}

impl<'a> State<'a> {
    fn new(cfg: &'a Layered, graph: &'a Graph) -> Self {
        let mut ports = HashMap::new();
        for (i, node) in graph.nodes.iter().enumerate() {
            for (list, output) in [(&node.inputs, false), (&node.outputs, true)] {
                for port in list {
                    ports.insert(
                        port.id,
                        PortRef {
                            node: i,
                            output,
                            y: port.y,
                        },
                    );
                }
            }
        }

        let mut index = BTreeMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut self_links = Vec::new();
        for (i, link) in graph.links.iter().enumerate() {
            let (Some(a), Some(b)) = (ports.get(&link.from), ports.get(&link.to)) else {
                continue;
            };
            if !a.output || b.output {
                continue;
            }
            if a.node == b.node {
                self_links.push(i);
                continue;
            }
            let e = *index.entry((a.node, b.node)).or_insert_with(|| {
                edges.push(Edge {
                    from: a.node,
                    to: b.node,
                    links: Vec::new(),
                    reversed: false,
                    from_y: 0.0,
                    to_y: 0.0,
                    chain: Vec::new(),
                });
                edges.len() - 1
            });
            edges[e].links.push(i);
        }
        for edge in &mut edges {
            let key = |&i: &usize| {
                let link = &graph.links[i];
                (ports[&link.from].y, ports[&link.to].y)
            };
            edge.links
                .sort_by(|a, b| key(a).partial_cmp(&key(b)).unwrap());
            let n = edge.links.len() as f64;
            edge.from_y = edge.links.iter().map(|i| key(i).0).sum::<f64>() / n;
            edge.to_y = edge.links.iter().map(|i| key(i).1).sum::<f64>() / n;
        }

        Self {
            cfg,
            graph,
            ports,
            edges,
            self_links,
            pins: Vec::new(),
            hints: Vec::new(),
            layer: vec![None; graph.nodes.len()],
            verts: Vec::new(),
            node_vert: vec![usize::MAX; graph.nodes.len()],
            segs: Vec::new(),
            left: Vec::new(),
            right: Vec::new(),
            layers: Vec::new(),
        }
    }

    /// Depth-first search; every edge that closes a cycle is marked reversed.
    fn break_cycles(&mut self) {
        let n = self.graph.nodes.len();
        let mut out = vec![Vec::new(); n];
        let mut has_in = vec![false; n];
        for (i, edge) in self.edges.iter().enumerate() {
            out[edge.from].push(i);
            has_in[edge.to] = true;
        }
        // Start from sources so that they end up on the left.
        let roots = (0..n).filter(|&i| !has_in[i]).chain(0..n);
        let mut visit = vec![0u8; n];
        for root in roots {
            if visit[root] != 0 {
                continue;
            }
            visit[root] = 1;
            let mut stack = vec![(root, 0)];
            while let Some(&(u, i)) = stack.last() {
                if i == out[u].len() {
                    visit[u] = 2;
                    stack.pop();
                    continue;
                }
                stack.last_mut().unwrap().1 += 1;
                let e = out[u][i];
                let v = self.edges[e].to;
                match visit[v] {
                    0 => {
                        visit[v] = 1;
                        stack.push((v, 0));
                    }
                    1 => self.edges[e].reversed = true,
                    _ => {}
                }
            }
        }
    }

    /// Longest-path layering, then each node moves towards the side it has
    /// more links on, which shortens long links.
    fn assign_layers(&mut self) {
        let n = self.graph.nodes.len();
        let mut preds = vec![Vec::new(); n];
        let mut succs = vec![Vec::new(); n];
        for edge in &self.edges {
            let (a, b) = edge.dag();
            succs[a].push((b, edge.links.len()));
            preds[b].push((a, edge.links.len()));
        }
        let mut layer = vec![0usize; n];
        let mut indeg: Vec<usize> = preds.iter().map(Vec::len).collect();
        let mut queue: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
        while let Some(u) = queue.pop() {
            for &(v, _) in &succs[u] {
                layer[v] = layer[v].max(layer[u] + 1);
                indeg[v] -= 1;
                if indeg[v] == 0 {
                    queue.push(v);
                }
            }
        }
        for _ in 0..n {
            let mut changed = false;
            for u in (0..n).filter(|&u| self.pins[u].is_none()) {
                let lo = preds[u]
                    .iter()
                    .map(|&(p, _)| layer[p] + 1)
                    .max()
                    .unwrap_or(0);
                let hi = succs[u].iter().map(|&(s, _)| layer[s] - 1).min();
                let pull_left: usize = preds[u].iter().map(|&(_, w)| w).sum();
                let pull_right: usize = succs[u].iter().map(|&(_, w)| w).sum();
                let target = match pull_right.cmp(&pull_left) {
                    std::cmp::Ordering::Greater => hi.unwrap_or(layer[u]),
                    std::cmp::Ordering::Less => lo,
                    std::cmp::Ordering::Equal => layer[u],
                };
                if target != layer[u] {
                    layer[u] = target;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let linked = |i: usize| !preds[i].is_empty() || !succs[i].is_empty();
        if self.pins.contains(&Some(Side::West)) {
            for u in 0..n {
                layer[u] = if self.pins[u] == Some(Side::West) {
                    0
                } else {
                    layer[u] + 1
                };
            }
        }
        let last = (0..n)
            .filter(|&i| linked(i))
            .map(|i| layer[i])
            .max()
            .unwrap_or(0);
        for u in (0..n).filter(|&u| self.pins[u] == Some(Side::East)) {
            layer[u] = last + 1;
        }
        let mut used: Vec<usize> = (0..n).filter(|&i| linked(i)).map(|i| layer[i]).collect();
        used.sort_unstable();
        used.dedup();
        for i in (0..n).filter(|&i| linked(i)) {
            self.layer[i] = used.binary_search(&layer[i]).ok();
        }
        self.layers = vec![Vec::new(); used.len()];
    }

    fn add_vert(&mut self, vert: Vert) -> usize {
        self.layers[vert.layer].push(self.verts.len());
        self.verts.push(vert);
        self.verts.len() - 1
    }

    fn build_verts(&mut self) {
        for i in 0..self.graph.nodes.len() {
            if let Some(layer) = self.layer[i] {
                let h = self.graph.nodes[i].size.h;
                self.node_vert[i] = self.add_vert(Vert {
                    node: Some(i),
                    layer,
                    h,
                    y: 0.0,
                    anchor: None,
                });
            }
        }
        for e in 0..self.edges.len() {
            let edge = &self.edges[e];
            let (from, to, reversed) = (edge.from, edge.to, edge.reversed);
            let (from_y, to_y) = (edge.from_y, edge.to_y);
            let w = edge.links.len() as f64;
            let h = (w - 1.0) * self.cfg.link_gap;
            let (a, b) = edge.dag();
            let (la, lb) = (self.layer[a].unwrap(), self.layer[b].unwrap());
            // A reversed edge leaves its source to the right, turns around and
            // travels back, so it needs a slot in the end columns as well.
            let range = if reversed { la..lb + 1 } else { la + 1..lb };
            let chain: Vec<usize> = range
                .map(|layer| {
                    self.add_vert(Vert {
                        node: None,
                        layer,
                        h,
                        y: 0.0,
                        anchor: None,
                    })
                })
                .collect();
            let mid = h / 2.0;
            let mut stops: Vec<(usize, f64)> = chain.iter().map(|&v| (v, mid)).collect();
            if reversed {
                self.verts[chain[0]].anchor = Some(self.node_vert[to]);
                self.verts[*chain.last().unwrap()].anchor = Some(self.node_vert[from]);
            } else {
                stops.insert(0, (self.node_vert[from], from_y));
                stops.push((self.node_vert[to], to_y));
            }
            for pair in stops.windows(2) {
                self.segs.push(Seg {
                    a: pair[0].0,
                    b: pair[1].0,
                    ya: pair[0].1,
                    yb: pair[1].1,
                    w,
                });
            }
            self.edges[e].chain = chain;
        }
        // Start each column in the order its nodes had before. New nodes and
        // link slots come last and are placed by the sweeps.
        let nodes = &self.graph.nodes;
        let verts = &self.verts;
        for layer in &mut self.layers {
            let before = |v: &usize| {
                verts[*v]
                    .node
                    .and_then(|n| nodes[n].before)
                    .map_or(f64::MAX, |p| p.y)
            };
            layer.sort_by(|a, b| before(a).partial_cmp(&before(b)).unwrap());
        }
        self.left = vec![Vec::new(); self.verts.len()];
        self.right = vec![Vec::new(); self.verts.len()];
        for seg in &self.segs {
            // Dummy-to-dummy segments count more, which keeps long links straight.
            let straight = self.verts[seg.a].node.is_none() && self.verts[seg.b].node.is_none();
            let w = if straight { seg.w * 4.0 } else { seg.w };
            self.right[seg.a].push(Nb {
                vert: seg.b,
                y_self: seg.ya,
                y_other: seg.yb,
                w,
            });
            self.left[seg.b].push(Nb {
                vert: seg.a,
                y_self: seg.yb,
                y_other: seg.ya,
                w,
            });
        }
    }

    fn positions(&self) -> Vec<usize> {
        let mut pos = vec![0; self.verts.len()];
        for layer in &self.layers {
            for (i, &v) in layer.iter().enumerate() {
                pos[v] = i;
            }
        }
        pos
    }

    /// Rank of a point on a vertex, in units of column slots.
    fn rank(&self, pos: &[usize], vert: usize, y: f64) -> f64 {
        let h = self.verts[vert].h;
        pos[vert] as f64
            + if h > 0.0 {
                (y / h).clamp(0.0, 1.0) * 0.9
            } else {
                0.45
            }
    }

    fn crossings(&self) -> f64 {
        let pos = self.positions();
        let mut by_gap = vec![Vec::new(); self.layers.len()];
        for seg in &self.segs {
            by_gap[self.verts[seg.a].layer].push((
                self.rank(&pos, seg.a, seg.ya),
                self.rank(&pos, seg.b, seg.yb),
                seg.w,
            ));
        }
        let mut total = 0.0;
        for gap in &by_gap {
            for (i, p) in gap.iter().enumerate() {
                for q in &gap[i + 1..] {
                    if (p.0 - q.0) * (p.1 - q.1) < 0.0 {
                        total += p.2 * q.2;
                    }
                }
            }
        }
        total
    }

    /// Barycentre sweeps, counting where on a node each link attaches.
    fn order(&mut self) {
        let count = self.layers.len();
        // Columns with a prescribed order are sorted once and then left alone.
        let mut fixed = vec![false; count];
        for l in 0..count {
            let hints: Option<HashMap<usize, f64>> = self.layers[l]
                .iter()
                .map(|&v| Some((v, self.hints[self.verts[v].node?]?)))
                .collect();
            if let Some(hints) = hints.filter(|h| !h.is_empty()) {
                self.layers[l].sort_by(|a, b| hints[a].partial_cmp(&hints[b]).unwrap());
                fixed[l] = true;
            }
        }
        let mut best = (self.crossings(), self.layers.clone());
        for sweep in 0..24 {
            let forward = sweep % 2 == 0;
            let columns: Vec<usize> = if forward {
                (1..count).collect()
            } else {
                (0..count.saturating_sub(1)).rev().collect()
            };
            for l in columns.into_iter().filter(|&l| !fixed[l]) {
                let pos = self.positions();
                let mut key = HashMap::new();
                for &v in &self.layers[l] {
                    let nbs = if forward {
                        &self.left[v]
                    } else {
                        &self.right[v]
                    };
                    let total: f64 = nbs.iter().map(|nb| nb.w).sum();
                    let k = if nbs.is_empty() {
                        pos[v] as f64
                    } else {
                        nbs.iter()
                            .map(|nb| nb.w * self.rank(&pos, nb.vert, nb.y_other))
                            .sum::<f64>()
                            / total
                    };
                    key.insert(v, k);
                }
                for &v in &self.layers[l] {
                    if let Some(anchor) = self.verts[v].anchor {
                        key.insert(v, key[&anchor] + 1e-6);
                    }
                }
                self.layers[l].sort_by(|a, b| key[a].partial_cmp(&key[b]).unwrap());
            }
            let crossings = self.crossings();
            if crossings < best.0 {
                best = (crossings, self.layers.clone());
            }
        }
        self.layers = best.1;
    }

    fn gap(&self, above: usize, below: usize) -> f64 {
        match (self.verts[above].node, self.verts[below].node) {
            (Some(_), Some(_)) => self.cfg.node_gap,
            (None, None) => self.cfg.link_gap,
            _ => self.cfg.node_gap * 0.6,
        }
    }

    /// Moves the vertices of one column as close as possible to where their
    /// neighbours want them, keeping their order and spacing.
    fn place(&mut self, l: usize, use_left: bool, use_right: bool) {
        let layer = &self.layers[l];
        // (wanted position minus the space taken by everything above, weight)
        let mut wants = Vec::with_capacity(layer.len());
        let mut offsets = Vec::with_capacity(layer.len());
        let mut offset = 0.0;
        for (i, &v) in layer.iter().enumerate() {
            if i > 0 {
                offset += self.verts[layer[i - 1]].h + self.gap(layer[i - 1], v);
            }
            let mut sum = 0.0;
            let mut weight = 0.0;
            for (nbs, used) in [(&self.left[v], use_left), (&self.right[v], use_right)] {
                for nb in nbs.iter().filter(|_| used) {
                    sum += nb.w * (self.verts[nb.vert].y + nb.y_other - nb.y_self);
                    weight += nb.w;
                }
            }
            let want = if weight > 0.0 {
                sum / weight
            } else {
                self.verts[v].y
            };
            wants.push((want - offset, weight.max(0.01)));
            offsets.push(offset);
        }
        // Weighted isotonic regression (pool adjacent violators).
        let mut blocks: Vec<(f64, f64, usize)> = Vec::new();
        for &(z, w) in &wants {
            blocks.push((z * w, w, 1));
            while blocks.len() > 1 {
                let (b, a) = (blocks[blocks.len() - 1], blocks[blocks.len() - 2]);
                if a.0 / a.1 <= b.0 / b.1 {
                    break;
                }
                blocks.truncate(blocks.len() - 2);
                blocks.push((a.0 + b.0, a.1 + b.1, a.2 + b.2));
            }
        }
        let mut i = 0;
        for (sum, weight, len) in blocks {
            for _ in 0..len {
                self.verts[self.layers[l][i]].y = sum / weight + offsets[i];
                i += 1;
            }
        }
    }

    fn assign_y(&mut self) {
        let count = self.layers.len();
        for l in 0..count {
            let mut y = 0.0;
            for i in 0..self.layers[l].len() {
                let v = self.layers[l][i];
                if i > 0 {
                    y += self.gap(self.layers[l][i - 1], v);
                }
                self.verts[v].y = y;
                y += self.verts[v].h;
            }
        }
        for _ in 0..10 {
            for l in 1..count {
                self.place(l, true, false);
            }
            for l in (0..count.saturating_sub(1)).rev() {
                self.place(l, false, true);
            }
        }
        for _ in 0..8 {
            for l in 0..count {
                self.place(l, true, true);
            }
        }
        self.close_bands();
    }

    /// Parts of the graph that share no links drift apart vertically; this
    /// closes every horizontal band that nothing occupies.
    fn close_bands(&mut self) {
        let mut order: Vec<usize> = (0..self.verts.len()).collect();
        order.sort_by(|&a, &b| self.verts[a].y.partial_cmp(&self.verts[b].y).unwrap());
        let keep = self.cfg.node_gap * 2.0;
        let mut bottom = f64::MIN;
        let mut shift = 0.0;
        for v in order {
            let vert = &mut self.verts[v];
            if bottom > f64::MIN && vert.y - bottom > keep {
                shift += vert.y - bottom - keep;
            }
            bottom = bottom.max(vert.y + vert.h);
            vert.y -= shift;
        }
    }

    fn finish(self) -> Layout {
        let cfg = self.cfg;
        let nodes = &self.graph.nodes;
        let count = self.layers.len();

        // Columns are as wide as their widest node; gaps grow with the number
        // of links that bend in them.
        let mut width = vec![0.0f64; count];
        for vert in &self.verts {
            if let Some(n) = vert.node {
                width[vert.layer] = width[vert.layer].max(nodes[n].size.w);
            }
        }
        let mut busy = vec![0.0; count];
        for seg in &self.segs {
            busy[self.verts[seg.a].layer] += seg.w;
        }
        let mut left = vec![0.0; count];
        for l in 1..count {
            let gap = cfg.layer_gap + (busy[l - 1] * 3.0).min(cfg.layer_gap * 2.0);
            left[l] = left[l - 1] + width[l - 1] + gap;
        }
        let right = |l: usize| left[l] + width[l];

        let mut at = vec![Point::default(); nodes.len()];
        let mut bottom = 0.0f64;
        for vert in &self.verts {
            if let Some(n) = vert.node {
                // Left-aligned, so that a column can be read straight down.
                at[n] = Point {
                    x: left[vert.layer],
                    y: vert.y,
                };
            }
            bottom = bottom.max(vert.y + vert.h);
        }

        // Nodes without links go in rows below everything else.
        let loose: f64 = (0..nodes.len())
            .filter(|&n| self.layer[n].is_none())
            .map(|n| nodes[n].size.w + cfg.node_gap)
            .sum();
        let row_width = count
            .checked_sub(1)
            .map_or(0.0, right)
            .max(loose.min(1200.0));
        let mut cursor = Point {
            x: 0.0,
            y: if count > 0 {
                bottom + cfg.node_gap * 3.0
            } else {
                0.0
            },
        };
        let mut row_h = 0.0f64;
        for (n, node) in nodes.iter().enumerate() {
            if self.layer[n].is_some() {
                continue;
            }
            if cursor.x > 0.0 && cursor.x + node.size.w > row_width {
                cursor = Point {
                    x: 0.0,
                    y: cursor.y + row_h + cfg.node_gap * 1.5,
                };
                row_h = 0.0;
            }
            at[n] = cursor;
            cursor.x += node.size.w + cfg.node_gap;
            row_h = row_h.max(node.size.h);
        }

        let port = |id: Id| {
            let port = &self.ports[&id];
            let node = &nodes[port.node];
            let x = at[port.node].x + if port.output { node.size.w } else { 0.0 };
            Point {
                x,
                y: at[port.node].y + port.y,
            }
        };

        let mut links = HashMap::new();
        for edge in &self.edges {
            let lu = self.layer[edge.from].unwrap();
            let lv = self.layer[edge.to].unwrap();
            for (j, &i) in edge.links.iter().enumerate() {
                let link = &self.graph.links[i];
                let (start, end) = (port(link.from), port(link.to));
                let lane = |v: usize| self.verts[v].y + j as f64 * cfg.link_gap;
                let bend = (cfg.layer_gap * 0.5 + j as f64 * cfg.link_gap).min(cfg.layer_gap);
                let mut pen = Pen::new(start);
                pen.to(
                    Point {
                        x: right(lu),
                        y: start.y,
                    },
                    1.0,
                    bend,
                );
                if edge.reversed {
                    for &v in edge.chain.iter().rev() {
                        let l = self.verts[v].layer;
                        pen.to(
                            Point {
                                x: right(l),
                                y: lane(v),
                            },
                            -1.0,
                            bend,
                        );
                        pen.to(
                            Point {
                                x: left[l],
                                y: lane(v),
                            },
                            -1.0,
                            bend,
                        );
                    }
                } else {
                    for &v in &edge.chain {
                        let l = self.verts[v].layer;
                        pen.to(
                            Point {
                                x: left[l],
                                y: lane(v),
                            },
                            1.0,
                            bend,
                        );
                        pen.to(
                            Point {
                                x: right(l),
                                y: lane(v),
                            },
                            1.0,
                            bend,
                        );
                    }
                }
                pen.to(
                    Point {
                        x: left[lv],
                        y: end.y,
                    },
                    1.0,
                    bend,
                );
                pen.to(end, 1.0, bend);
                links.insert(link.id, pen.path);
            }
        }
        // A link from a node to itself loops over the top of the node.
        for &i in &self.self_links {
            let link = &self.graph.links[i];
            let (start, end) = (port(link.from), port(link.to));
            let node = self.ports[&link.from].node;
            let over = at[node].y - cfg.node_gap * 0.4;
            let mut pen = Pen::new(start);
            pen.to(
                Point {
                    x: start.x,
                    y: over,
                },
                -1.0,
                18.0,
            );
            pen.to(Point { x: end.x, y: over }, -1.0, 18.0);
            pen.to(end, 1.0, 18.0);
            links.insert(link.id, pen.path);
        }

        let link_from: HashMap<Id, Id> = self.graph.links.iter().map(|l| (l.id, l.from)).collect();
        let link_to: HashMap<Id, Id> = self.graph.links.iter().map(|l| (l.id, l.to)).collect();
        // Move everything so that the bounding box starts at the origin.
        let empty = Rect {
            min: Point {
                x: f64::MAX,
                y: f64::MAX,
            },
            max: Point {
                x: f64::MIN,
                y: f64::MIN,
            },
        };
        let grow = |r: &mut Rect, p: Point| {
            r.min = Point {
                x: r.min.x.min(p.x),
                y: r.min.y.min(p.y),
            };
            r.max = Point {
                x: r.max.x.max(p.x),
                y: r.max.y.max(p.y),
            };
        };
        let (mut all, mut linked) = (empty, empty);
        for (n, node) in nodes.iter().enumerate() {
            let corner = Point {
                x: at[n].x + node.size.w,
                y: at[n].y + node.size.h,
            };
            for rect in [Some(&mut all), self.layer[n].map(|_| &mut linked)] {
                if let Some(rect) = rect {
                    grow(rect, at[n]);
                    grow(rect, corner);
                }
            }
        }
        for (id, path) in &links {
            let between_nodes = self.ports[&link_from[id]].node != self.ports[&link_to[id]].node;
            let points = path.curves.iter().flat_map(|c| [c.c1, c.c2, c.end]);
            for p in points.chain([path.start]) {
                grow(&mut all, p);
                if between_nodes {
                    grow(&mut linked, p);
                }
            }
        }
        if count == 0 {
            linked = all;
        }
        let min = all.min;
        let max = all.max;
        if nodes.is_empty() {
            return Layout::default();
        }
        let shift = |p: &mut Point| {
            *p = Point {
                x: p.x - min.x,
                y: p.y - min.y,
            }
        };
        at.iter_mut().for_each(shift);
        for path in links.values_mut() {
            shift(&mut path.start);
            for curve in &mut path.curves {
                shift(&mut curve.c1);
                shift(&mut curve.c2);
                shift(&mut curve.end);
            }
        }
        Layout {
            nodes: nodes.iter().zip(at).map(|(node, p)| (node.id, p)).collect(),
            links,
            groups: HashMap::new(),
            size: Size {
                w: max.x - min.x,
                h: max.y - min.y,
            },
            linked: Rect {
                min: Point {
                    x: linked.min.x - min.x,
                    y: linked.min.y - min.y,
                },
                max: Point {
                    x: linked.max.x - min.x,
                    y: linked.max.y - min.y,
                },
            },
        }
    }
}

/// Draws a path out of curves whose ends are always horizontal.
struct Pen {
    path: Path,
    at: Point,
    /// Horizontal direction of travel at `at`: 1 is rightwards.
    dir: f64,
}

impl Pen {
    fn new(start: Point) -> Self {
        Self {
            path: Path {
                start,
                curves: Vec::new(),
            },
            at: start,
            dir: 1.0,
        }
    }

    /// Continues to `end`, arriving there travelling in direction `dir`.
    /// `bend` is how far a U-turn swings out.
    fn to(&mut self, end: Point, dir: f64, bend: f64) {
        if end == self.at && dir == self.dir {
            return;
        }
        let reach = if dir == self.dir {
            (end.x - self.at.x).abs() / 2.0
        } else {
            bend
        };
        self.path.curves.push(Curve {
            c1: Point {
                x: self.at.x + self.dir * reach,
                y: self.at.y,
            },
            c2: Point {
                x: end.x - dir * reach,
                y: end.y,
            },
            end,
        });
        self.at = end;
        self.dir = dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Group, Link, Node, Port};

    fn node(id: Id, inputs: u32, outputs: u32) -> Node {
        let port = |base: u32| {
            move |i: u32| Port {
                id: id * 100 + base + i,
                y: 30.0 + 16.0 * i as f64,
                label: String::new(),
            }
        };
        Node {
            id,
            group: None,
            before: None,
            size: Size {
                w: 120.0,
                h: 30.0 + 16.0 * inputs.max(outputs).max(1) as f64,
            },
            inputs: (0..inputs).map(port(0)).collect(),
            outputs: (0..outputs).map(port(50)).collect(),
            label: String::new(),
        }
    }

    /// Link from output `o` of node `a` to input `i` of node `b`.
    fn link(id: Id, a: Id, o: u32, b: Id, i: u32) -> Link {
        Link {
            id,
            from: a * 100 + 50 + o,
            to: b * 100 + i,
        }
    }

    fn cubic(p: [Point; 4], t: f64) -> Point {
        let u = 1.0 - t;
        let f = |a: f64, b: f64, c: f64, d: f64| {
            u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
        };
        Point {
            x: f(p[0].x, p[1].x, p[2].x, p[3].x),
            y: f(p[0].y, p[1].y, p[2].y, p[3].y),
        }
    }

    /// Lays the graph out and checks what must hold for every layout.
    fn check(graph: &Graph) -> Layout {
        let layout = Layered::default().layout(graph);
        assert_eq!(layout.nodes.len(), graph.nodes.len());
        let rect = |n: &Node| {
            let p = layout.nodes[&n.id];
            (p.x, p.y, p.x + n.size.w, p.y + n.size.h)
        };
        for (i, a) in graph.nodes.iter().enumerate() {
            let a = rect(a);
            for b in &graph.nodes[i + 1..] {
                let b = rect(b);
                assert!(
                    a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1,
                    "nodes overlap"
                );
            }
        }
        // Every node lies inside all of its groups and outside all others.
        let parent: HashMap<Id, Option<Id>> =
            graph.groups.iter().map(|g| (g.id, g.parent)).collect();
        let around = |mut at: Option<Id>| {
            let mut chain = Vec::new();
            while let Some(g) = at {
                chain.push(g);
                at = parent[&g];
            }
            chain
        };
        assert_eq!(layout.groups.len(), graph.groups.len());
        for n in &graph.nodes {
            let (r, chain) = (rect(n), around(n.group));
            for (id, g) in &layout.groups {
                if chain.contains(id) {
                    assert!(
                        r.0 >= g.min.x && r.1 >= g.min.y && r.2 <= g.max.x && r.3 <= g.max.y,
                        "node {} outside its group {id}",
                        n.id
                    );
                } else {
                    assert!(
                        r.2 <= g.min.x || g.max.x <= r.0 || r.3 <= g.min.y || g.max.y <= r.1,
                        "node {} overlaps group {id}",
                        n.id
                    );
                }
            }
        }
        for a in &graph.groups {
            for b in &graph.groups {
                let (ra, rb) = (layout.groups[&a.id], layout.groups[&b.id]);
                if around(Some(a.id)).contains(&b.id) {
                    assert!(
                        ra.min.x >= rb.min.x && ra.max.x <= rb.max.x,
                        "group outside its parent"
                    );
                    assert!(
                        ra.min.y >= rb.min.y && ra.max.y <= rb.max.y,
                        "group outside its parent"
                    );
                } else if !around(Some(b.id)).contains(&a.id) {
                    assert!(
                        ra.max.x <= rb.min.x
                            || rb.max.x <= ra.min.x
                            || ra.max.y <= rb.min.y
                            || rb.max.y <= ra.min.y,
                        "groups {} and {} overlap",
                        a.id,
                        b.id
                    );
                }
            }
        }
        for link in &graph.links {
            let valid = graph
                .nodes
                .iter()
                .any(|n| n.outputs.iter().any(|p| p.id == link.from))
                && graph
                    .nodes
                    .iter()
                    .any(|n| n.inputs.iter().any(|p| p.id == link.to));
            let Some(path) = layout.links.get(&link.id) else {
                assert!(!valid, "link {} was not routed", link.id);
                continue;
            };
            let mut from = path.start;
            for curve in &path.curves {
                for step in 0..=32 {
                    let p = cubic([from, curve.c1, curve.c2, curve.end], step as f64 / 32.0);
                    assert!(p.x >= -1e-6 && p.y >= -1e-6, "link leaves the bounds");
                    assert!(p.x <= layout.size.w + 1e-6 && p.y <= layout.size.h + 1e-6);
                    for n in &graph.nodes {
                        let r = rect(n);
                        let inside = p.x > r.0 + 0.5
                            && p.x < r.2 - 0.5
                            && p.y > r.1 + 0.5
                            && p.y < r.3 - 0.5;
                        assert!(!inside, "link {} crosses node {}", link.id, n.id);
                    }
                }
                from = curve.end;
            }
            let ends = graph.nodes.iter().flat_map(|n| {
                let r = rect(n);
                let inputs = n.inputs.iter().map(move |p| {
                    (
                        p.id,
                        Point {
                            x: r.0,
                            y: r.1 + p.y,
                        },
                    )
                });
                inputs.chain(n.outputs.iter().map(move |p| {
                    (
                        p.id,
                        Point {
                            x: r.2,
                            y: r.1 + p.y,
                        },
                    )
                }))
            });
            let ends: HashMap<Id, Point> = ends.collect();
            let near = |a: Point, b: Point| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6;
            assert!(
                near(path.start, ends[&link.from]),
                "link does not start at its port"
            );
            assert!(near(from, ends[&link.to]), "link does not end at its port");
        }
        layout
    }

    #[test]
    fn empty() {
        check(&Graph::default());
    }

    #[test]
    fn chain_runs_left_to_right() {
        let graph = Graph {
            groups: Vec::new(),
            nodes: vec![node(3, 1, 0), node(1, 0, 1), node(2, 1, 1)],
            links: vec![link(1, 1, 0, 2, 0), link(2, 2, 0, 3, 0)],
        };
        let layout = check(&graph);
        assert!(layout.nodes[&1].x < layout.nodes[&2].x);
        assert!(layout.nodes[&2].x < layout.nodes[&3].x);
        // Nothing forces a bend, so the links are straight.
        assert_eq!(layout.nodes[&1].y, layout.nodes[&2].y);
    }

    #[test]
    fn cycles_and_self_links() {
        let graph = Graph {
            groups: Vec::new(),
            nodes: vec![node(1, 2, 2), node(2, 2, 2), node(3, 1, 1)],
            links: vec![
                link(1, 1, 0, 2, 0),
                link(2, 1, 1, 2, 1),
                link(3, 2, 0, 1, 0),
                link(4, 2, 1, 1, 1),
                link(5, 3, 0, 3, 0),
            ],
        };
        check(&graph);
    }

    #[test]
    fn long_link_avoids_nodes() {
        let graph = Graph {
            groups: Vec::new(),
            nodes: vec![node(1, 0, 2), node(2, 1, 1), node(3, 1, 1), node(4, 2, 0)],
            links: vec![
                link(1, 1, 0, 2, 0),
                link(2, 2, 0, 3, 0),
                link(3, 3, 0, 4, 0),
                link(4, 1, 1, 4, 1),
            ],
        };
        check(&graph);
    }

    #[test]
    fn linked_bounds_exclude_unlinked_nodes() {
        let mut nodes = vec![node(1, 0, 1), node(2, 1, 0)];
        nodes.extend((3..40).map(|id| node(id, 1, 1)));
        let graph = Graph {
            groups: Vec::new(),
            nodes,
            links: vec![link(1, 1, 0, 2, 0)],
        };
        let layout = check(&graph);
        let (a, b) = (layout.nodes[&1], layout.nodes[&2]);
        assert_eq!(layout.linked.min, a);
        assert_eq!(layout.linked.max.x, b.x + 120.0);
        assert!(layout.linked.max.y < layout.nodes[&3].y);
        assert!(layout.linked.size().h < layout.size.h);
    }

    #[test]
    fn link_between_nested_groups() {
        let group = |id, parent| Group {
            id,
            parent,
            header: Size { w: 40.0, h: 18.0 },
        };
        let mut nodes = vec![node(1, 0, 2), node(2, 2, 2), node(3, 2, 0), node(4, 1, 1)];
        nodes[0].group = Some(11);
        nodes[1].group = Some(11);
        nodes[2].group = Some(20);
        let graph = Graph {
            groups: vec![group(10, None), group(11, Some(10)), group(20, None)],
            nodes,
            links: vec![
                link(1, 1, 0, 2, 0),
                link(2, 1, 1, 2, 1),
                link(3, 2, 0, 3, 0),
                link(4, 2, 1, 3, 1),
                link(5, 2, 0, 4, 0),
                link(6, 4, 0, 1, 0),
            ],
        };
        let layout = check(&graph);
        assert_eq!(
            layout.links.len(),
            5,
            "link 6 targets a port that does not exist"
        );
        assert!(layout.groups[&10].max.x <= layout.groups[&20].min.x);
    }

    #[test]
    fn group_follows_the_order_of_its_sources() {
        // Four sources in one group, each feeding one sink in another group,
        // in scrambled order.
        let mut nodes: Vec<Node> = (1..=4).map(|id| node(id, 0, 1)).collect();
        nodes.extend((5..=8).map(|id| node(id, 1, 0)));
        for (i, node) in nodes.iter_mut().enumerate() {
            node.group = Some(if i < 4 { 1 } else { 2 });
        }
        let group = |id| Group {
            id,
            parent: None,
            header: Size { w: 40.0, h: 18.0 },
        };
        let graph = Graph {
            groups: vec![group(1), group(2)],
            nodes,
            links: vec![
                link(1, 1, 0, 7, 0),
                link(2, 2, 0, 5, 0),
                link(3, 3, 0, 8, 0),
                link(4, 4, 0, 6, 0),
            ],
        };
        let layout = check(&graph);
        for (from, to) in [(1, 7), (2, 5), (3, 8), (4, 6)] {
            assert_eq!(
                layout.nodes[&from].y, layout.nodes[&to].y,
                "{from} -> {to} is not straight"
            );
        }
    }

    #[test]
    fn stereo_pair_runs_parallel() {
        let graph = Graph {
            groups: Vec::new(),
            nodes: vec![node(1, 0, 2), node(2, 2, 0)],
            links: vec![link(1, 1, 0, 2, 0), link(2, 1, 1, 2, 1)],
        };
        let layout = check(&graph);
        let (a, b) = (&layout.links[&1], &layout.links[&2]);
        assert_eq!(a.curves.len(), b.curves.len());
        assert_eq!(b.start.y - a.start.y, 16.0);
    }

    #[test]
    fn random_graphs() {
        let mut seed = 12345u64;
        let mut next = |n: u32| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as u32) % n
        };
        for _ in 0..40 {
            let count = 2 + next(25);
            // Up to five groups, each possibly nested in an earlier one.
            let groups: Vec<Group> = (0..next(6))
                .map(|id| Group {
                    id,
                    parent: (id > 0 && next(2) == 0).then(|| next(id)),
                    header: Size { w: 60.0, h: 18.0 },
                })
                .collect();
            let mut nodes: Vec<Node> = (1..=count).map(|id| node(id, next(4), next(4))).collect();
            for node in &mut nodes {
                if !groups.is_empty() && next(3) > 0 {
                    node.group = Some(next(groups.len() as u32));
                }
            }
            let mut links = Vec::new();
            for id in 0..next(60) {
                let (a, b) = (&nodes[next(count) as usize], &nodes[next(count) as usize]);
                if a.outputs.is_empty() || b.inputs.is_empty() {
                    continue;
                }
                let from = a.outputs[next(a.outputs.len() as u32) as usize].id;
                let to = b.inputs[next(b.inputs.len() as u32) as usize].id;
                if !links.iter().any(|l: &Link| l.from == from && l.to == to) {
                    links.push(Link { id, from, to });
                }
            }
            let graph = Graph {
                nodes,
                links,
                groups,
            };
            let first = check(&graph);
            // Laying out again from the result changes nothing.
            let mut again = graph.clone();
            for node in &mut again.nodes {
                node.before = Some(first.nodes[&node.id]);
            }
            let settled = Layered::default().layout(&again);
            let far = |a: &Layout, b: &Layout| {
                let moves = graph.nodes.iter().map(|n| {
                    let (p, q) = (a.nodes[&n.id], b.nodes[&n.id]);
                    (p.x - q.x).abs().max((p.y - q.y).abs())
                });
                moves.fold(0.0, f64::max)
            };
            for node in &mut again.nodes {
                node.before = Some(settled.nodes[&node.id]);
            }
            let third = Layered::default().layout(&again);
            // Seeded with its own result a layout may shift once, and then
            // stays where it is.
            let _ = far(&first, &settled);
            assert_eq!(
                far(&settled, &third),
                0.0,
                "layout keeps moving without a change"
            );
            let second = Layered::default().layout(&graph);
            assert_eq!(first.nodes, second.nodes);
            assert_eq!(first.links, second.links);
        }
    }
}
