//! Decides which nodes are shown and which group each one belongs to.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde::Deserialize;

use crate::graph::{Direction, Graph, Id, Link, Node, Port};

/// `~/.config/tanglewire/config.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

/// Applies to nodes whose properties match every pattern in `match`. The first
/// matching rule wins.
#[derive(Debug, Deserialize)]
pub struct Rule {
    /// Node property name to glob pattern (`*` matches any text).
    #[serde(rename = "match")]
    pub matches: BTreeMap<String, String>,
    /// Slash-separated group path, e.g. `mic/fx`.
    pub group: Option<String>,
    pub hide: Option<bool>,
}

impl Config {
    pub fn path() -> PathBuf {
        gtk::glib::user_config_dir().join("tanglewire/config.toml")
    }

    /// A missing file is an empty configuration.
    pub fn load() -> Result<Self, String> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn rule_for(&self, node: &Node) -> Option<&Rule> {
        self.rules.iter().find(|rule| {
            rule.matches
                .iter()
                .all(|(key, pattern)| node.prop(key).is_some_and(|v| glob(pattern, v)))
        })
    }
}

/// The sound server and the session manager make nodes on behalf of others:
/// that such a client made a node says nothing about the node.
const SERVERS: [&str; 4] = [
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    "pipewire-media-session",
];

#[derive(Debug, Default)]
pub struct Grouping {
    pub hidden: HashSet<Id>,
    /// Group path of every grouped node, outermost first.
    pub paths: HashMap<Id, Vec<String>>,
}

/// With `reveal`, hidden nodes get a group like any other, for when they are
/// shown all the same.
pub fn resolve(graph: &Graph, config: &Config, reveal: bool) -> Grouping {
    let mut out = Grouping::default();
    let mut ungrouped = Vec::new();
    for node in graph.nodes.values() {
        let rule = config.rule_for(node);
        // Nodes without a media class are internals (drivers, EasyEffects plumbing).
        let hide = rule
            .and_then(|r| r.hide)
            .unwrap_or(node.prop("media.class").is_none());
        if hide {
            out.hidden.insert(node.id);
            if !reveal {
                continue;
            }
        }
        let path = node
            .prop("patchbay.group")
            .or(rule.and_then(|r| r.group.as_deref()));
        match path {
            Some(path) => {
                let path: Vec<String> = path
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                if !path.is_empty() {
                    out.paths.insert(node.id, path);
                }
            }
            None => ungrouped.push(node),
        }
    }

    // Automatic groups. The two halves of a loopback or filter chain belong
    // together, and so do the nodes of one device; a chain attached to a
    // device nests inside the device's group. What is left is grouped by
    // application, or by name if several nodes share one. Not every
    // application names itself on its nodes: failing that, the name it gave
    // when it connected counts.
    let own = |key: &'static str| move |n: &Node| n.prop(key).map(str::to_owned);
    let name_of = |members: &[&Node], fallback: String| {
        let names: Vec<&str> = members.iter().map(|n| n.name()).collect();
        common_prefix(&names).unwrap_or(fallback)
    };

    let chains = bucket(&ungrouped, &own("node.link-group"));
    let chain_of = |n: &Node| n.prop("node.link-group").and_then(|key| chains.get(key));
    let device_of = |n: &Node| {
        let own = n.prop("device.id");
        let via_chain = || chain_of(n)?.iter().find_map(|m| m.prop("device.id"));
        own.or_else(via_chain).map(str::to_owned)
    };
    let devices = bucket(&ungrouped, &device_of);
    for (id, members) in &devices {
        // Named after the nodes that are the device's own.
        let own: Vec<&Node> = members
            .iter()
            .copied()
            .filter(|n| n.prop("device.id").is_some())
            .collect();
        let described = members
            .iter()
            .find_map(|n| n.device(graph)?.get("device.description").cloned());
        let name = described.unwrap_or_else(|| name_of(&own, format!("Device {id}")));
        // No frame around a device that is nothing but one chain.
        if chain_of(members[0]).is_some_and(|c| c.len() == members.len()) {
            continue;
        }
        for node in members {
            out.paths.insert(node.id, vec![name.clone()]);
        }
    }
    for members in chains.values() {
        let name = name_of(members, members[0].name().to_owned());
        for node in members {
            out.paths.entry(node.id).or_default().push(name.clone());
        }
    }
    ungrouped.retain(|n| !out.paths.contains_key(&n.id));
    let apps = bucket(&ungrouped, &|n: &Node| {
        let client = || {
            let client = n.client(graph)?;
            let binary = client.get("application.process.binary");
            let server = binary.is_some_and(|b| SERVERS.contains(&b.as_str()));
            (!server)
                .then(|| client.get("application.name"))?
                .map(String::as_str)
        };
        n.prop("application.name")
            .or_else(client)
            .or(n.prop("node.name"))
            .map(str::to_owned)
    });
    for (key, members) in &apps {
        let name = name_of(members, key.clone());
        for node in members {
            out.paths.insert(node.id, vec![name.clone()]);
        }
    }
    out
}

/// `name` as it reads inside the groups of `path` (outermost first): without
/// what the title of a group around it says already. The innermost group
/// that leaves something of the name decides.
pub fn short(name: &str, path: &[String]) -> String {
    let rest = path.iter().rev().map(|group| without(name, group));
    rest.into_iter()
        .find(|rest| !rest.is_empty())
        .unwrap_or_else(|| name.to_owned())
}

/// `name` without `group` wherever that stands as words of their own, and
/// without the punctuation this leaves at the ends.
fn without(name: &str, group: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut rest = String::new();
    let mut from = 0;
    while let Some(at) = (!group.is_empty())
        .then(|| name[from..].find(group))
        .flatten()
    {
        let (start, end) = (from + at, from + at + group.len());
        rest.push_str(&name[from..start]);
        // Not part of a longer word: `Mic` is not taken out of `Microphone`.
        if word(name[..start].chars().next_back()) || word(name[end..].chars().next()) {
            rest.push_str(group);
        }
        from = end;
    }
    rest.push_str(&name[from..]);
    let edge = |c: char| c.is_whitespace() || ":-/,_|".contains(c);
    let mut rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    loop {
        let trimmed = rest.trim_matches(edge);
        // What was in brackets to tell it from the group's name is the name now.
        let inner = trimmed
            .strip_prefix('(')
            .and_then(|r| r.strip_suffix(')'))
            .filter(|r| !r.contains(['(', ')']));
        let next = inner.unwrap_or(trimmed).trim_matches(edge);
        if next.len() == rest.len() {
            return rest;
        }
        rest = next.to_owned();
    }
}

/// A route from one shown port to another by way of hidden nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Through {
    pub out_node: Id,
    pub out_port: Id,
    pub in_node: Id,
    pub in_port: Id,
    /// The hidden nodes on the way, in order.
    pub via: Vec<Id>,
}

/// What is connected through hidden nodes: every pair of shown ports with a
/// chain of hidden nodes between them and no link of their own. What happens
/// inside a hidden node is a guess, see `passes`.
pub fn through(graph: &Graph, hidden: &HashSet<Id>) -> Vec<Through> {
    let mut from: HashMap<Id, Vec<&Link>> = HashMap::new();
    for link in graph.links.values() {
        from.entry(link.out_port).or_default().push(link);
    }
    let mut found: HashSet<(Id, Id)> = graph
        .links
        .values()
        .map(|l| (l.out_port, l.in_port))
        .collect();
    let mut out = Vec::new();
    let enters = |l: &&Link| !hidden.contains(&l.out_node) && hidden.contains(&l.in_node);
    for start in graph.links.values().filter(enters) {
        // Hidden inputs that have been followed; chains may loop.
        let mut seen = HashSet::new();
        let mut open = vec![(start.in_port, vec![start.in_node])];
        while let Some((port, via)) = open.pop() {
            let Some(input) = graph.ports.get(&port).filter(|_| seen.insert(port)) else {
                continue;
            };
            let links = passes(graph, input)
                .into_iter()
                .flat_map(|output| from.get(&output.id).into_iter().flatten());
            for link in links {
                if hidden.contains(&link.in_node) {
                    let mut via = via.clone();
                    if !via.contains(&link.in_node) {
                        via.push(link.in_node);
                    }
                    open.push((link.in_port, via));
                } else if found.insert((start.out_port, link.in_port)) {
                    out.push(Through {
                        out_node: start.out_node,
                        out_port: start.out_port,
                        in_node: link.in_node,
                        in_port: link.in_port,
                        via: via.clone(),
                    });
                }
            }
        }
    }
    out.sort_by_key(|t| (t.out_port, t.in_port));
    out
}

/// The outputs of a node that an input of it presumably feeds: those of the
/// same channel, or without channels the one in the same position, else all.
/// A side input such as `PROBE_FL` goes with the channel it is named after.
fn passes<'a>(graph: &'a Graph, input: &Port) -> Vec<&'a Port> {
    let outputs = graph.ports_of(input.node, Direction::Out);
    let named_after = |channel: &str, other: &str| {
        let rest = channel.strip_suffix(other);
        rest.is_some_and(|r| r.is_empty() || r.ends_with(|c: char| !c.is_alphanumeric()))
    };
    let same: Vec<&Port> = match &input.channel {
        Some(channel) => outputs
            .iter()
            .copied()
            .filter(|p| (p.channel.as_deref()).is_some_and(|other| named_after(channel, other)))
            .collect(),
        None => outputs
            .iter()
            .copied()
            .filter(|p| p.index == input.index)
            .collect(),
    };
    if same.is_empty() { outputs } else { same }
}

/// Nodes by `key`, leaving out keys that only one node has.
fn bucket<'a>(
    nodes: &[&'a Node],
    key: &dyn Fn(&Node) -> Option<String>,
) -> BTreeMap<String, Vec<&'a Node>> {
    let mut buckets: BTreeMap<String, Vec<&'a Node>> = BTreeMap::new();
    for &node in nodes {
        if let Some(key) = key(node) {
            buckets.entry(key).or_default().push(node);
        }
    }
    buckets.retain(|_, members| members.len() > 1);
    buckets
}

/// The words all names start with, if that is at least three characters.
fn common_prefix(names: &[&str]) -> Option<String> {
    let first = names.first()?;
    let mut len = first.len();
    for name in names {
        len = first
            .bytes()
            .zip(name.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count();
    }
    while !first.is_char_boundary(len) {
        len -= 1;
    }
    let mut prefix = &first[..len];
    // Do not stop in the middle of a word.
    if names
        .iter()
        .any(|n| n[len..].chars().next().is_some_and(char::is_alphanumeric))
    {
        prefix = &prefix[..prefix.rfind(|c: char| !c.is_alphanumeric()).unwrap_or(0)];
    }
    let prefix = prefix.trim_end_matches(|c: char| !c.is_alphanumeric() && c != ')');
    (prefix.chars().count() >= 3).then(|| prefix.to_owned())
}

/// Matches `text` against a pattern in which `*` stands for any text.
fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let Some(last) = parts.next_back() else {
        return rest.is_empty();
    };
    for part in parts {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Event;

    fn graph(nodes: &[(Id, &[(&str, &str)])]) -> Graph {
        let mut graph = Graph::default();
        for (id, props) in nodes {
            let props = props
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            graph.apply(Event::Node(Node { id: *id, props }));
        }
        graph
    }

    #[test]
    fn names_lose_what_the_group_says() {
        let path =
            |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
        let device = path(&["Scarlett Solo 4th Gen"]);
        let chain = path(&["Scarlett Solo 4th Gen", "Scarlett Solo 4th Gen Input 2 Mic"]);
        assert_eq!(
            short("Scarlett Solo 4th Gen Headphones / Line 1-2", &device),
            "Headphones / Line 1-2"
        );
        assert_eq!(short("Scarlett Solo 4th Gen 0", &device), "0");
        // The group's own name is not shortened to nothing.
        assert_eq!(
            short("Scarlett Solo 4th Gen", &device),
            "Scarlett Solo 4th Gen"
        );
        assert_eq!(short("Valve Index", &device), "Valve Index");
        // Anywhere in the name, and the next group out if nothing is left.
        assert_eq!(
            short("Split Scarlett Solo 4th Gen Input 2 Mic", &chain),
            "Split"
        );
        assert_eq!(
            short("Scarlett Solo 4th Gen Input 2 Mic", &chain),
            "Input 2 Mic"
        );
        assert_eq!(
            short("Monitor Input 3+4 (stereo)", &device),
            "Monitor Input 3+4 (stereo)"
        );
        // What told twins apart becomes the name.
        assert_eq!(
            short("OBS (Desktop Audio)", &path(&["OBS"])),
            "Desktop Audio"
        );
        assert_eq!(short("obs #429", &path(&["obs"])), "#429");
        assert_eq!(
            short(
                "MicProcessing (MicProcessing output)",
                &path(&["MicProcessing"])
            ),
            "output"
        );
        // Only whole words.
        assert_eq!(short("Microphone", &path(&["Mic"])), "Microphone");
        assert_eq!(short("Mic: Microphone", &path(&["Mic"])), "Microphone");
        assert_eq!(short("Alone", &[]), "Alone");
    }

    #[test]
    fn globs() {
        assert!(glob("tmp_*", "tmp_level_meter"));
        assert!(glob("*", ""));
        assert!(glob("Audio/*", "Audio/Sink"));
        assert!(glob("a*b*c", "a-b-b-c"));
        assert!(glob("exact", "exact"));
        assert!(!glob("exact", "exactly"));
        assert!(!glob("a*b", "a-b-c"));
        assert!(!glob("ab*b", "ab"));
    }

    #[test]
    fn prefixes() {
        assert_eq!(
            common_prefix(&["Vox FX Chain Sink", "Vox FX Chain Output"]).unwrap(),
            "Vox FX Chain"
        );
        assert_eq!(common_prefix(&["OBS", "OBS"]).unwrap(), "OBS");
        assert_eq!(
            common_prefix(&["Scarlett Solo Input", "Scarlett Solid"]).unwrap(),
            "Scarlett"
        );
        assert_eq!(common_prefix(&["Mic", "Loopback"]), None);
    }

    #[test]
    fn chain_nests_in_its_device() {
        let sink = ("media.class", "Audio/Sink");
        let graph = graph(&[
            (
                1,
                &[sink, ("node.description", "Card Out"), ("device.id", "7")],
            ),
            (
                2,
                &[
                    sink,
                    ("node.description", "Card In"),
                    ("device.id", "7"),
                    ("node.link-group", "g"),
                ],
            ),
            (
                3,
                &[
                    sink,
                    ("node.description", "Split Card In"),
                    ("node.link-group", "g"),
                ],
            ),
            (
                4,
                &[
                    sink,
                    ("node.description", "Solo"),
                    ("device.id", "8"),
                    ("node.link-group", "h"),
                ],
            ),
            (
                5,
                &[
                    sink,
                    ("node.description", "Solo Split"),
                    ("node.link-group", "h"),
                ],
            ),
        ]);
        let grouping = resolve(&graph, &Config::default(), false);
        assert_eq!(grouping.paths[&1], ["Card"]);
        assert_eq!(grouping.paths[&2], ["Card", "Card In"]);
        assert_eq!(grouping.paths[&3], ["Card", "Card In"]);
        assert_eq!(grouping.paths[&4], ["Solo"]);
        assert_eq!(grouping.paths[&5], ["Solo"]);
    }

    #[test]
    fn nodes_of_one_client_belong_together() {
        let client = ("client.id", "9");
        let sink = || [("media.class".to_owned(), "Audio/Sink".to_owned())];
        let mut graph = graph(&[
            (
                1,
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.description", "Effects Sink"),
                    client,
                ],
            ),
            (
                2,
                &[
                    ("media.class", "Audio/Source"),
                    ("node.description", "Effects Source"),
                    client,
                ],
            ),
            (3, &[("node.description", "Gate"), client]),
            // Says itself what it belongs to.
            (
                4,
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.description", "Other"),
                    ("application.name", "other"),
                    client,
                ],
            ),
        ]);
        let props = [("application.name".to_owned(), "effects".to_owned())];
        graph.apply(Event::Client(9, props.into()));
        // Two sinks made by the session manager have nothing in common.
        for (id, name) in [(5, "Left"), (6, "Right")] {
            let props = [("node.description", name), ("client.id", "8")];
            let props = props.map(|(k, v)| (k.to_owned(), v.to_owned()));
            graph.apply(Event::Node(Node {
                id,
                props: props.into_iter().chain(sink()).collect(),
            }));
        }
        let props = [
            ("application.name", "WirePlumber"),
            ("application.process.binary", "wireplumber"),
        ];
        let props = props.map(|(k, v)| (k.to_owned(), v.to_owned()));
        graph.apply(Event::Client(8, props.into()));
        let grouping = resolve(&graph, &Config::default(), false);
        assert_eq!(grouping.hidden, HashSet::from([3]));
        assert_eq!(grouping.paths[&1], ["Effects"]);
        assert_eq!(grouping.paths[&2], ["Effects"]);
        assert!(!grouping.paths.contains_key(&3));
        assert!(!grouping.paths.contains_key(&4));
        assert!(!grouping.paths.contains_key(&5));
        assert!(!grouping.paths.contains_key(&6));
        // Shown all the same, the hidden node joins the others.
        let grouping = resolve(&graph, &Config::default(), true);
        assert_eq!(grouping.hidden, HashSet::from([3]));
        assert_eq!(grouping.paths[&3], ["effects"]);
        assert_eq!(grouping.paths[&1], ["effects"]);
    }

    /// Nodes 1 to 5 in a row with stereo ports: node `n` has inputs `n0 0`
    /// and `n0 1`, outputs `n0 5` and `n0 6`.
    fn chain(links: &[(Id, Id)]) -> Graph {
        let mut graph = graph(&[(1, &[]), (2, &[]), (3, &[]), (4, &[]), (5, &[])]);
        for node in 1..=5 {
            for (at, direction) in [(0, Direction::In), (5, Direction::Out)] {
                for (index, channel) in ["FL", "FR"].into_iter().enumerate() {
                    graph.apply(Event::Port(Port {
                        id: node * 100 + at + index as Id,
                        node,
                        name: channel.to_owned(),
                        direction,
                        index: index as u32,
                        channel: Some(channel.to_owned()),
                        format: None,
                        monitor: false,
                        props: Default::default(),
                    }));
                }
            }
        }
        for (id, (out_port, in_port)) in links.iter().enumerate() {
            graph.apply(Event::Link(Link {
                id: 1000 + id as Id,
                out_node: out_port / 100,
                out_port: *out_port,
                in_node: in_port / 100,
                in_port: *in_port,
            }));
        }
        graph
    }

    fn ends(through: &[Through]) -> Vec<(Id, Id)> {
        through.iter().map(|t| (t.out_port, t.in_port)).collect()
    }

    #[test]
    fn hidden_chains_are_bridged_channel_by_channel() {
        // 1 -> 2 -> 3 -> 4 in stereo, 2 and 3 hidden; 3 also feeds itself.
        let graph = chain(&[
            (105, 200),
            (106, 201),
            (205, 300),
            (206, 301),
            (305, 400),
            (306, 401),
            (305, 300),
        ]);
        let found = through(&graph, &HashSet::from([2, 3]));
        assert_eq!(ends(&found), [(105, 400), (106, 401)]);
        assert_eq!(found[0].via, [2, 3]);
        assert!(through(&graph, &HashSet::new()).is_empty());
    }

    #[test]
    fn bridges_fan_out_and_leave_direct_links_alone() {
        // 1 feeds hidden 2, which feeds 4 and 5; 1 is linked to 4 as well.
        // The right output of 1 ends in the hidden node 3 and goes nowhere.
        let graph = chain(&[(105, 200), (205, 400), (205, 500), (105, 400), (106, 301)]);
        let found = through(&graph, &HashSet::from([2, 3]));
        assert_eq!(ends(&found), [(105, 500)]);
    }

    #[test]
    fn side_inputs_go_with_their_channel() {
        // 1 feeds the inputs of hidden 2 and, as a side chain, of hidden 3
        // behind it.
        let mut graph = chain(&[
            (105, 200),
            (106, 201),
            (205, 300),
            (206, 301),
            (305, 400),
            (306, 401),
        ]);
        for (index, channel) in ["PROBE_FL", "PROBE_FR"].into_iter().enumerate() {
            let id = 302 + index as Id;
            let mut probe = graph.ports[&300].clone();
            (probe.id, probe.index) = (id, 2 + index as u32);
            probe.channel = Some(channel.to_owned());
            graph.apply(Event::Port(probe));
            graph.apply(Event::Link(Link {
                id: 2000 + id,
                out_node: 1,
                out_port: 105 + index as Id,
                in_node: 3,
                in_port: id,
            }));
        }
        let found = through(&graph, &HashSet::from([2, 3]));
        assert_eq!(ends(&found), [(105, 400), (106, 401)]);
    }

    #[test]
    fn without_a_matching_channel_every_output_is_fed() {
        let mut graph = chain(&[(105, 200), (205, 400), (206, 401)]);
        graph.ports.get_mut(&200).unwrap().channel = Some("MONO".to_owned());
        let found = through(&graph, &HashSet::from([2]));
        assert_eq!(ends(&found), [(105, 400), (105, 401)]);
    }

    #[test]
    fn sources_in_order_of_precedence() {
        let graph = graph(&[
            (
                1,
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.name", "a"),
                    ("patchbay.group", "x/y"),
                ],
            ),
            (
                2,
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.name", "amp_in"),
                    ("node.link-group", "g"),
                ],
            ),
            (
                3,
                &[
                    ("media.class", "Audio/Source"),
                    ("node.name", "amp_out"),
                    ("node.link-group", "g"),
                ],
            ),
            (4, &[("node.name", "driver")]),
            (
                5,
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.name", "alone"),
                    ("device.id", "7"),
                ],
            ),
            (6, &[("media.class", "Audio/Sink"), ("node.name", "ruled")]),
            (7, &[("node.name", "shown-internal")]),
        ]);
        let config: Config = toml::from_str(
            r#"
            [[rule]]
            match = { "node.name" = "ruled" }
            group = "mine"
            [[rule]]
            match = { "node.name" = "shown-*" }
            hide = false
            "#,
        )
        .unwrap();
        let grouping = resolve(&graph, &config, false);
        assert_eq!(grouping.hidden, HashSet::from([4]));
        assert_eq!(grouping.paths[&1], ["x", "y"]);
        assert_eq!(grouping.paths[&2], ["amp"]);
        assert_eq!(grouping.paths[&3], ["amp"]);
        assert_eq!(grouping.paths[&6], ["mine"]);
        assert!(!grouping.paths.contains_key(&5));
        assert!(!grouping.paths.contains_key(&7));
    }
}
