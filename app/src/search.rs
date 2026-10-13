//! Finds nodes by name, port, id or any other property.
//!
//! Plain text is matched loosely: as part of a word, with a typo, with
//! letters left out, or by sound. `/…/` is a regular expression.

use regex::{Regex, RegexBuilder};

use crate::graph::{Graph, Id};

pub enum Query {
    /// Lower-case words that must all match.
    Words(Vec<String>),
    Regex(Regex),
}

/// How well a node matches, and which of its ports do.
#[derive(Debug, Default, PartialEq)]
pub struct Match {
    pub score: u32,
    pub ports: Vec<Id>,
}

impl Query {
    /// `None` for an empty query.
    pub fn parse(text: &str) -> Result<Option<Self>, String> {
        let text = text.trim();
        if let Some(pattern) = text.strip_prefix('/') {
            let pattern = pattern.strip_suffix('/').unwrap_or(pattern);
            if pattern.is_empty() {
                return Ok(None);
            }
            let regex = RegexBuilder::new(pattern).case_insensitive(true).build();
            return regex
                .map(|r| Some(Self::Regex(r)))
                .map_err(|e| e.to_string());
        }
        let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
        Ok((!words.is_empty()).then_some(Self::Words(words)))
    }

    /// Scores `text`; `loose` allows typos, left-out letters and sound-alikes.
    fn score(&self, text: &str, loose: bool) -> Option<u32> {
        match self {
            Self::Regex(regex) => regex.is_match(text).then_some(80),
            Self::Words(words) => {
                let text = text.to_lowercase();
                // Every word has to be found; the weakest one decides.
                words
                    .iter()
                    .map(|w| word_score(w, &text, loose))
                    .min()
                    .flatten()
            }
        }
    }

    pub fn matches(&self, graph: &Graph, node: Id) -> Option<Match> {
        let node = graph.nodes.get(&node)?;
        let mut found = Match::default();
        let mut consider = |score: Option<u32>, weight: u32| {
            if let Some(score) = score {
                found.score = found.score.max(score * weight / 10);
            }
            score.is_some()
        };
        consider(self.score(node.name(), true), 10);
        consider(self.score(node.prop("node.name").unwrap_or(""), true), 9);
        // Ids match as a whole only: "5" should not find every id with a 5 in it.
        let whole = |id: &str| match self {
            Self::Words(words) => words.len() == 1 && words[0] == id,
            Self::Regex(regex) => regex.is_match(id),
        };
        consider(whole(&node.id.to_string()).then_some(100), 10);
        // Everything else PipeWire says: for the port its alias, channel and
        // format, for the node its application, process, media name, ALSA
        // card, serial and so on. Matched as written, also as `key=value`.
        // A bare number means an id, and matches a property only as a whole.
        let number = matches!(self, Self::Words(words) if words.iter().all(|w| w.chars().all(|c| c.is_ascii_digit())));
        let props = |props: &crate::graph::Props| {
            let pairs = props.iter().map(|(key, value)| {
                if number {
                    return whole(value).then_some(100);
                }
                self.score(value, false)
                    .or_else(|| self.score(&format!("{key}={value}"), false))
            });
            pairs.max().flatten()
        };
        for port in graph.ports.values().filter(|p| p.node == node.id) {
            let by_id = whole(&port.id.to_string()).then_some(100);
            let named = consider(self.score(&port.name, true).max(by_id), 8);
            if consider(props(&port.props), 6) || named {
                found.ports.push(port.id);
            }
        }
        consider(props(&node.props), 6);
        // The device (vendor, product, bus, card) and the client (process,
        // user, host) the node belongs to.
        for owner in [node.device(graph), node.client(graph)]
            .into_iter()
            .flatten()
        {
            consider(props(owner), 6);
        }
        (found.score > 0).then_some(found)
    }
}

/// How well one query word matches a piece of lower-case text.
fn word_score(word: &str, text: &str, loose: bool) -> Option<u32> {
    if text.is_empty() {
        return None;
    }
    if text == word {
        return Some(100);
    }
    let parts: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.iter().any(|p| p.starts_with(word)) {
        return Some(90);
    }
    if text.contains(word) {
        return Some(80);
    }
    let len = word.chars().count();
    if !loose || len < 3 {
        return None;
    }
    // A typo: one wrong letter, two in a long word.
    let allowed = if len >= 8 { 2 } else { 1 };
    let typo = |p: &&str| {
        let head: String = p.chars().take(len + 1).collect();
        len >= 4 && (distance(word, p) <= allowed || distance(word, &head) <= allowed)
    };
    if parts.iter().any(typo) {
        return Some(60);
    }
    // Letters left out: "sclt" finds "scarlett".
    let skips = |p: &&str| {
        let mut letters = p.chars();
        p.starts_with(&word[..word.chars().next().map_or(0, char::len_utf8)])
            && word.chars().all(|c| letters.any(|l| l == c))
    };
    if parts.iter().any(skips) {
        return Some(50);
    }
    let sound = soundex(word);
    (sound.is_some() && parts.iter().any(|p| soundex(p) == sound)).then_some(40)
}

/// Edit distance (insert, delete, replace).
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let replaced = diagonal + usize::from(ca != *cb);
            diagonal = row[j + 1];
            row[j + 1] = replaced.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// American Soundex: words that sound alike get the same code.
fn soundex(word: &str) -> Option<String> {
    let code = |c: char| match c.to_ascii_lowercase() {
        'b' | 'f' | 'p' | 'v' => '1',
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2',
        'd' | 't' => '3',
        'l' => '4',
        'm' | 'n' => '5',
        'r' => '6',
        'h' | 'w' => '-',
        _ => '0',
    };
    let mut letters = word.chars().filter(char::is_ascii_alphabetic);
    let first = letters.next()?;
    let mut out = String::from(first.to_ascii_uppercase());
    let mut last = code(first);
    for c in letters {
        let now = code(c);
        // h and w do not separate two letters with the same code.
        if now == '-' {
            continue;
        }
        if now != '0' && now != last {
            out.push(now);
        }
        last = now;
    }
    out.truncate(4);
    while out.len() < 4 {
        out.push('0');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Direction, Event, Node, Port};

    fn graph() -> Graph {
        let mut graph = Graph::default();
        let node = |id, name: &str, extra: &[(&str, &str)]| {
            let mut props: crate::graph::Props = extra
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            props.insert("node.description".into(), name.into());
            Event::Node(Node { id, props })
        };
        let port = |id, node, name: &str| {
            Event::Port(Port {
                id,
                node,
                name: name.into(),
                direction: Direction::Out,
                index: id,
                channel: None,
                format: None,
                monitor: false,
                props: [("port.alias".to_owned(), format!("alias-{id}"))].into(),
            })
        };
        for event in [
            node(50, "Scarlett Solo 4th Gen", &[("device.id", "7")]),
            node(
                51,
                "Bitwig Studio",
                &[("application.process.binary", "bitwig-studio")],
            ),
            node(52, "Monitor", &[("client.id", "9")]),
            port(500, 50, "capture_AUX0"),
            port(520, 52, "playback_FL"),
            Event::Device(
                7,
                [("device.vendor.name".to_owned(), "Focusrite".to_owned())].into(),
            ),
            Event::Client(
                9,
                [("application.process.user".to_owned(), "rain".to_owned())].into(),
            ),
        ] {
            graph.apply(event);
        }
        graph
    }

    fn hits(query: &str) -> Vec<Id> {
        let graph = graph();
        let query = Query::parse(query).unwrap().unwrap();
        graph
            .nodes
            .keys()
            .copied()
            .filter(|&id| query.matches(&graph, id).is_some())
            .collect()
    }

    #[test]
    fn plain_text() {
        assert_eq!(hits("scar"), [50]);
        assert_eq!(hits("studio bit"), [51]);
        assert_eq!(hits("SOLO"), [50]);
        assert!(hits("zzz").is_empty());
    }

    #[test]
    fn loose_text() {
        assert_eq!(hits("scarlet"), [50], "typo");
        assert_eq!(hits("bitwgi"), [51], "typo");
        assert_eq!(hits("sclt"), [50], "letters left out");
        assert_eq!(hits("skarlit"), [50], "by sound");
        assert_eq!(hits("monitar"), [52], "by sound or typo");
    }

    #[test]
    fn ports_ids_and_properties() {
        let graph = graph();
        let found = |q: &str, id| Query::parse(q).unwrap().unwrap().matches(&graph, id);
        assert_eq!(found("aux0", 50).unwrap().ports, [500]);
        assert_eq!(found("500", 50).unwrap().ports, [500]);
        assert_eq!(hits("52"), [52]);
        assert_eq!(hits("5"), [], "ids match as a whole");
        assert_eq!(hits("focusrite"), [50], "device vendor");
        assert_eq!(hits("rain"), [52], "client property");
        assert_eq!(
            found("alias-520", 52).unwrap().ports,
            [520],
            "port property"
        );
        assert_eq!(hits("bitwig-studio"), [51], "process binary");
        assert_eq!(hits("device.id=7"), [50], "any property as key=value");
    }

    #[test]
    fn regular_expressions() {
        assert_eq!(hits("/^(bit|mon)/"), [51, 52]);
        assert_eq!(hits("/playback_F[LR]$/"), [52]);
        assert!(Query::parse("/(/").is_err());
        assert!(Query::parse("  ").unwrap().is_none());
    }

    #[test]
    fn better_matches_score_higher() {
        let graph = graph();
        let score = |q: &str, id| {
            Query::parse(q)
                .unwrap()
                .unwrap()
                .matches(&graph, id)
                .unwrap()
                .score
        };
        assert!(score("monitor", 52) > score("mon", 52));
        assert!(score("mon", 52) > score("monitar", 52));
    }

    #[test]
    fn soundex_codes() {
        assert_eq!(soundex("Robert").unwrap(), "R163");
        assert_eq!(soundex("Rupert").unwrap(), "R163");
        assert_eq!(soundex("Ashcraft").unwrap(), "A261");
        assert_eq!(soundex("Tymczak").unwrap(), "T522");
        assert_eq!(soundex("123"), None);
    }
}
