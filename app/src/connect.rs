//! Works out which links a drag from one thing to another should make or remove.

use crate::graph::{Direction, Graph, Id, Port};
use crate::pw::Command;

/// What a drag starts on or ends on.
#[derive(Debug, Clone, PartialEq)]
pub enum End {
    Ports(Vec<Id>),
    Node(Id),
}

/// Links to create or remove for a drag from `from` to `to`. If every pair of
/// ports is already linked the links are removed, otherwise the missing ones
/// are created.
pub fn plan(graph: &Graph, from: &End, to: &End) -> Vec<Command> {
    let pairs = pairs(graph, from, to);
    let existing = |&(out, inp): &(Id, Id)| {
        graph
            .links
            .values()
            .find(|l| l.out_port == out && l.in_port == inp)
            .map(|l| l.id)
    };
    if !pairs.is_empty() && pairs.iter().all(|p| existing(p).is_some()) {
        return pairs
            .iter()
            .filter_map(existing)
            .map(Command::Unlink)
            .collect();
    }
    let missing = pairs.into_iter().filter(|p| existing(p).is_none());
    missing
        .filter_map(|(out, inp)| {
            Some(Command::Link {
                out_node: graph.ports.get(&out)?.node,
                out_port: out,
                in_node: graph.ports.get(&inp)?.node,
                in_port: inp,
            })
        })
        .collect()
}

/// One line saying what a drag from `from` does: what is being dragged while
/// it is over nothing, and what dropping it on `to` would do.
pub fn explain(graph: &Graph, from: &End, to: Option<&End>) -> String {
    let node = |id: &Id| graph.label(*id);
    let name = |end: &End| match end {
        End::Node(id) => node(id),
        End::Ports(ports) => match ports.as_slice() {
            [one] => graph
                .ports
                .get(one)
                .map_or("?".into(), |p| format!("{} {}", node(&p.node), p.name)),
            [first, ..] => {
                let owner = graph
                    .ports
                    .get(first)
                    .map_or("?".to_owned(), |p| node(&p.node));
                format!("{} ports of {owner}", ports.len())
            }
            [] => "?".into(),
        },
    };
    let Some(to) = to else {
        return format!("Linking {} …", name(from));
    };
    let plan = plan(graph, from, to);
    let port = |id: &Id| {
        graph
            .ports
            .get(id)
            .map_or("?".into(), |p| format!("{} {}", node(&p.node), p.name))
    };
    match plan.as_slice() {
        [] => format!("Cannot link {} and {}", name(from), name(to)),
        // One link: name the two ports it is about, whatever was dragged.
        [
            Command::Link {
                out_port, in_port, ..
            },
        ] => format!("Link {} → {}", port(out_port), port(in_port)),
        [Command::Unlink(id)] => match graph.links.get(id) {
            Some(link) => format!("Unlink {} → {}", port(&link.out_port), port(&link.in_port)),
            None => "Unlink".to_owned(),
        },
        [Command::Unlink(_), ..] => format!(
            "Unlink {} → {} ({} links)",
            name(from),
            name(to),
            plan.len()
        ),
        [Command::Link { .. }, ..] => {
            format!("Link {} → {} ({} links)", name(from), name(to), plan.len())
        }
    }
}

/// Output and input port of every link the drag stands for.
fn pairs(graph: &Graph, from: &End, to: &End) -> Vec<(Id, Id)> {
    let ports =
        |ids: &[Id]| -> Vec<&Port> { ids.iter().filter_map(|id| graph.ports.get(id)).collect() };
    let side = |node: Id, direction: Direction| {
        let all = graph.ports_of(node, direction);
        // A sink's monitor ports only stand in when it has no real outputs.
        let real: Vec<&Port> = all.iter().copied().filter(|p| !p.monitor).collect();
        if real.is_empty() { all } else { real }
    };
    let (a, b) = match (from, to) {
        (End::Ports(a), End::Ports(b)) => {
            let (a, b) = (ports(a), ports(b));
            match (a.as_slice(), b.as_slice()) {
                // Several ports dropped on one port go to that port's node.
                ([_, _, ..], [one]) => {
                    let b = side(one.node, one.direction);
                    (a, b)
                }
                _ => (a, b),
            }
        }
        (End::Ports(a), End::Node(node)) | (End::Node(node), End::Ports(a)) => {
            let a = ports(a);
            let Some(first) = a.first() else {
                return Vec::new();
            };
            let opposite = match first.direction {
                Direction::In => Direction::Out,
                Direction::Out => Direction::In,
            };
            let b = side(*node, opposite);
            (a, b)
        }
        (End::Node(a), End::Node(b)) => {
            let forward = (side(*a, Direction::Out), side(*b, Direction::In));
            if forward.0.is_empty() || forward.1.is_empty() {
                (side(*b, Direction::Out), side(*a, Direction::In))
            } else {
                forward
            }
        }
    };
    let (outs, ins): (Vec<&Port>, Vec<&Port>) = match (a.first(), b.first()) {
        (Some(x), Some(y)) if x.direction == Direction::Out && y.direction == Direction::In => {
            (a, b)
        }
        (Some(x), Some(y)) if x.direction == Direction::In && y.direction == Direction::Out => {
            (b, a)
        }
        _ => return Vec::new(),
    };
    let outs: Vec<&Port> = outs
        .into_iter()
        .filter(|p| p.direction == Direction::Out)
        .collect();
    let mut ins: Vec<&Port> = ins
        .into_iter()
        .filter(|p| p.direction == Direction::In)
        .collect();

    // Audio never goes to MIDI or video.
    let fits = |out: &Port, inp: &Port| {
        out.format.is_none() || inp.format.is_none() || out.format == inp.format
    };
    let mut pairs = Vec::new();
    let mut rest = Vec::new();
    // Same channel first (FL to FL), then whatever is left, in order.
    for out in outs {
        let same = ins
            .iter()
            .position(|i| out.channel.is_some() && i.channel == out.channel && fits(out, i));
        match same {
            Some(at) => pairs.push((out.id, ins.remove(at).id)),
            None => rest.push(out),
        }
    }
    for out in rest {
        if let Some(at) = ins.iter().position(|i| fits(out, i)) {
            pairs.push((out.id, ins.remove(at).id));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Event, Link};

    fn port(id: Id, node: Id, direction: Direction, channel: &str) -> Event {
        Event::Port(Port {
            id,
            node,
            name: channel.into(),
            direction,
            index: id,
            channel: (!channel.is_empty()).then(|| channel.into()),
            format: Some(
                if channel == "midi" {
                    "8 bit raw midi"
                } else {
                    "32 bit float mono audio"
                }
                .into(),
            ),
            monitor: false,
            props: Default::default(),
        })
    }

    /// Node 1: outputs 10 FL, 11 FR. Node 2: inputs 20 FR, 21 FL (swapped).
    /// Node 3: inputs 30, 31 without channels, 32 MIDI.
    fn graph() -> Graph {
        let mut graph = Graph::default();
        for event in [
            port(10, 1, Direction::Out, "FL"),
            port(11, 1, Direction::Out, "FR"),
            port(20, 2, Direction::In, "FR"),
            port(21, 2, Direction::In, "FL"),
            port(32, 3, Direction::In, "midi"),
            port(30, 3, Direction::In, ""),
            port(31, 3, Direction::In, ""),
        ] {
            graph.apply(event);
        }
        graph
    }

    fn links(commands: &[Command]) -> Vec<(Id, Id)> {
        let link = |c: &Command| match c {
            Command::Link {
                out_port, in_port, ..
            } => Some((*out_port, *in_port)),
            Command::Unlink(_) => None,
        };
        commands.iter().filter_map(link).collect()
    }

    #[test]
    fn explains_what_a_drop_does() {
        let mut graph = graph();
        for (id, name) in [(1, "Desktop"), (2, "Monitor")] {
            let props = [("node.description".to_owned(), name.to_owned())].into();
            graph.apply(Event::Node(crate::graph::Node { id, props }));
        }
        let (a, b) = (End::Node(1), End::Node(2));
        assert_eq!(explain(&graph, &a, None), "Linking Desktop …");
        assert_eq!(
            explain(&graph, &End::Ports(vec![10]), None),
            "Linking Desktop FL …"
        );
        assert_eq!(
            explain(&graph, &a, Some(&b)),
            "Link Desktop → Monitor (2 links)"
        );
        assert_eq!(
            explain(&graph, &End::Ports(vec![10]), Some(&End::Ports(vec![11]))),
            "Cannot link Desktop FL and Desktop FR"
        );
        // A title dropped on one port: the port that pairs with it is named,
        // in signal direction even when dragged the other way.
        assert_eq!(
            explain(&graph, &a, Some(&End::Ports(vec![20]))),
            "Link Desktop FR → Monitor FR"
        );
        assert_eq!(
            explain(&graph, &b, Some(&End::Ports(vec![10]))),
            "Link Desktop FL → Monitor FL"
        );
        let link = |id, out_port, in_port| {
            Event::Link(Link {
                id,
                out_node: 1,
                out_port,
                in_node: 2,
                in_port,
            })
        };
        graph.apply(link(100, 10, 21));
        graph.apply(link(101, 11, 20));
        assert_eq!(
            explain(&graph, &a, Some(&b)),
            "Unlink Desktop → Monitor (2 links)"
        );
        assert_eq!(
            explain(&graph, &End::Ports(vec![10]), Some(&End::Ports(vec![21]))),
            "Unlink Desktop FL → Monitor FL"
        );
    }

    #[test]
    fn nodes_pair_by_channel() {
        let plan = plan(&graph(), &End::Node(1), &End::Node(2));
        assert_eq!(links(&plan), [(10, 21), (11, 20)]);
    }

    #[test]
    fn drag_direction_does_not_matter() {
        let plan = plan(&graph(), &End::Node(2), &End::Node(1));
        assert_eq!(links(&plan), [(10, 21), (11, 20)]);
    }

    #[test]
    fn without_channels_ports_pair_in_order_and_skip_midi() {
        let plan = plan(&graph(), &End::Node(1), &End::Node(3));
        assert_eq!(links(&plan), [(10, 30), (11, 31)]);
    }

    #[test]
    fn single_port_to_single_port() {
        let plan = plan(&graph(), &End::Ports(vec![20]), &End::Ports(vec![10]));
        assert_eq!(links(&plan), [(10, 20)]);
        assert!(plan_is_empty(&End::Ports(vec![10]), &End::Ports(vec![11])));
        assert!(plan_is_empty(&End::Ports(vec![10]), &End::Ports(vec![32])));
    }

    fn plan_is_empty(from: &End, to: &End) -> bool {
        plan(&graph(), from, to).is_empty()
    }

    #[test]
    fn selection_dropped_on_one_port_spreads_over_the_node() {
        let plan = plan(&graph(), &End::Ports(vec![10, 11]), &End::Ports(vec![20]));
        assert_eq!(links(&plan), [(10, 21), (11, 20)]);
    }

    #[test]
    fn only_missing_links_are_made_and_a_full_set_is_removed() {
        let mut graph = graph();
        let link = |id, out_port, in_port| {
            Event::Link(Link {
                id,
                out_node: 1,
                out_port,
                in_node: 2,
                in_port,
            })
        };
        graph.apply(link(100, 10, 21));
        assert_eq!(
            links(&plan(&graph, &End::Node(1), &End::Node(2))),
            [(11, 20)]
        );
        graph.apply(link(101, 11, 20));
        let plan = plan(&graph, &End::Node(1), &End::Node(2));
        assert!(matches!(
            plan[..],
            [Command::Unlink(100), Command::Unlink(101)]
        ));
    }
}
