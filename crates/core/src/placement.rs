//! Placement: which sandcastle node a computer runs on (docs/self-host.md,
//! seam 2). A deployment that runs its computers on nodes lists them in
//! `FRAGMENT_NODES`, a JSON object:
//!
//! ```json
//! { "nodes": [
//!     { "id": "box", "url": "http://192.168.50.7:9400", "arch": "x86_64", "capacity": 32 },
//!     { "id": "mac", "uplink": true, "arch": "aarch64", "capacity": 8 } ],
//!   "images": {
//!     "stub": { "x86_64": "docker.io/library/fragment-stub:3", "aarch64": "docker.io/library/fragment-stub:3-arm64" },
//!     "hermes": "registry.example/fragment-hermes:7" } }
//! ```
//!
//! - **A node**: its id; how the platform reaches it (its API's `url`, or
//!   `uplink` when it dials the platform); its architecture; and its
//!   capacity, the computers it holds. Its secret is the Worker secret
//!   `secret_name(id)` (docs/secrets.md), never in this variable.
//! - **An image**, by the name computers are pinned to: one reference for
//!   every architecture (a multi-arch index, or a fleet of one
//!   architecture), or one per architecture.
//!
//! A computer is placed at its first start, and stays on that node for its
//! life: its container and its snapshots are the node's. The rule
//! (`rank`): of the nodes that answer, report the architecture they are
//! listed with, hold the computer's image for it, and have room, the one
//! holding the fewest computers for its capacity; a tie goes to the node
//! listed first (the deployment's preference). A node's count never falls:
//! computers are not deleted, and moving one to another node is not built.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The most nodes a deployment lists.
pub const NODES_MAX: usize = 64;
/// The most images it names.
pub const IMAGES_MAX: usize = 32;
/// A node's id, at most (sandcastle's `auth::NODE_ID_BYTES_MAX`).
pub const NODE_ID_BYTES_MAX: usize = 64;
/// The most computers one node holds.
pub const CAPACITY_MAX: u32 = 10_000;
/// A node's URL, at most.
pub const URL_BYTES_MAX: usize = 512;
/// An image's name, at most.
pub const IMAGE_NAME_BYTES_MAX: usize = 64;
/// An image's reference, at most.
pub const REFERENCE_BYTES_MAX: usize = 512;
/// A node's secret, at least (sandcastle's `auth::Secret`).
pub const SECRET_BYTES_MIN: usize = 32;

/// A node's architecture, as Rust (`std::env::consts::ARCH`) and the node's
/// health name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Arch {
    #[serde(rename = "x86_64")]
    X86_64,
    #[serde(rename = "aarch64")]
    Aarch64,
}

impl Arch {
    pub const ALL: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

    pub fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    pub fn parse(s: &str) -> Option<Arch> {
        Arch::ALL.into_iter().find(|a| a.name() == s)
    }
}

/// How the platform reaches a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Its API's origin, with no trailing slash.
    Url(String),
    /// It dials the platform (`/api/nodes/uplink`); its `Node` object holds
    /// the connection.
    Uplink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub reach: Reach,
    pub arch: Arch,
    /// The computers it holds.
    pub capacity: u32,
}

/// A deployment's nodes and images, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nodes {
    nodes: Vec<Node>,
    /// Each image's reference by architecture.
    images: BTreeMap<String, BTreeMap<Arch, String>>,
}

/// Why `FRAGMENT_NODES` is refused (a node with a bad one refuses its first
/// request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodesError {
    /// Not `{nodes, images}` as above.
    Shape(String),
    /// No nodes, or more than `NODES_MAX`.
    Count(usize),
    /// A node's id is not 1 to 64 of a-z, 0-9 and `-`.
    Id(String),
    /// Two nodes have one id.
    Duplicate(String),
    /// A node has both a URL and an uplink, or neither, or a URL that is no
    /// http(s) origin.
    Reach { node: String, why: String },
    /// A capacity of 0 or past `CAPACITY_MAX`.
    Capacity { node: String, capacity: u32 },
    /// No images, more than `IMAGES_MAX`, a bad name, or a bad reference.
    Image { image: String, why: String },
}

impl fmt::Display for NodesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodesError::Shape(e) => write!(f, "FRAGMENT_NODES is {{\"nodes\": [{{id, url | uplink, arch, capacity}}], \"images\": {{name: reference | {{arch: reference}}}}}}: {e}"),
            NodesError::Count(n) => write!(f, "1 to {NODES_MAX} nodes, not {n}"),
            NodesError::Id(id) => write!(f, "{id:?} is not a node's id (1 to {NODE_ID_BYTES_MAX} of a-z, 0-9 and -)"),
            NodesError::Duplicate(id) => write!(f, "the node {id} is listed twice"),
            NodesError::Reach { node, why } => write!(f, "the node {node}: {why}"),
            NodesError::Capacity { node, capacity } => write!(f, "the node {node}'s capacity is 1 to {CAPACITY_MAX} computers, not {capacity}"),
            NodesError::Image { image, why } => write!(f, "the image {image:?}: {why}"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    nodes: Vec<WireNode>,
    images: BTreeMap<String, WireRefs>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireNode {
    id: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    uplink: bool,
    arch: Arch,
    capacity: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireRefs {
    Every(String),
    ByArch(BTreeMap<Arch, String>),
}

/// A node's id: 1 to 64 of a-z, 0-9 and `-` (sandcastle's `valid_node_id`).
pub fn valid_node_id(id: &str) -> bool {
    (1..=NODE_ID_BYTES_MAX).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// An image's name, as computers are pinned to it.
fn valid_image_name(name: &str) -> bool {
    (1..=IMAGE_NAME_BYTES_MAX).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && name.as_bytes()[0].is_ascii_alphanumeric()
}

/// A reference as a node's engine takes it: printable, no spaces. The
/// engine parses it itself.
fn valid_reference(r: &str) -> bool {
    (1..=REFERENCE_BYTES_MAX).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_graphic())
}

/// The Worker secret that holds node `id`'s secret: `FRAGMENT_NODE_SECRET_`
/// and the id upper-cased, `-` as `_` (one-to-one: an id has no `_`).
pub fn secret_name(id: &str) -> String {
    assert!(valid_node_id(id), "a node's id is checked before its secret is named");
    format!("FRAGMENT_NODE_SECRET_{}", id.to_ascii_uppercase().replace('-', "_"))
}

/// An http(s) origin, its trailing slash dropped.
fn origin(url: &str) -> Result<String, String> {
    if url.len() > URL_BYTES_MAX {
        return Err(format!("a url of at most {URL_BYTES_MAX} bytes"));
    }
    let u = url::Url::parse(url).map_err(|e| format!("url: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() || u.path() != "/" || u.query().is_some() || u.fragment().is_some() || !u.username().is_empty() {
        return Err("url is an http(s) origin, with no path, query or credentials".into());
    }
    Ok(url.trim_end_matches('/').to_string())
}

impl Nodes {
    pub fn parse(json: &str) -> Result<Nodes, NodesError> {
        let wire: Wire = serde_json::from_str(json).map_err(|e| NodesError::Shape(e.to_string()))?;
        if wire.nodes.is_empty() || wire.nodes.len() > NODES_MAX {
            return Err(NodesError::Count(wire.nodes.len()));
        }
        let mut nodes: Vec<Node> = Vec::with_capacity(wire.nodes.len());
        for n in wire.nodes {
            if !valid_node_id(&n.id) {
                return Err(NodesError::Id(n.id));
            }
            if nodes.iter().any(|m| m.id == n.id) {
                return Err(NodesError::Duplicate(n.id));
            }
            let reach = match (n.url, n.uplink) {
                (Some(url), false) => Reach::Url(origin(&url).map_err(|why| NodesError::Reach { node: n.id.clone(), why })?),
                (None, true) => Reach::Uplink,
                (Some(_), true) => return Err(NodesError::Reach { node: n.id, why: "a url or an uplink, not both".into() }),
                (None, false) => return Err(NodesError::Reach { node: n.id, why: "a url, or \"uplink\": true for a node that dials in".into() }),
            };
            if n.capacity == 0 || n.capacity > CAPACITY_MAX {
                return Err(NodesError::Capacity { node: n.id, capacity: n.capacity });
            }
            nodes.push(Node { id: n.id, reach, arch: n.arch, capacity: n.capacity });
        }
        if wire.images.is_empty() || wire.images.len() > IMAGES_MAX {
            return Err(NodesError::Image { image: String::new(), why: format!("1 to {IMAGES_MAX} images, not {}", wire.images.len()) });
        }
        let mut images = BTreeMap::new();
        for (name, refs) in wire.images {
            let bad = |why: String| NodesError::Image { image: name.clone(), why };
            if !valid_image_name(&name) {
                return Err(bad(format!("a name is 1 to {IMAGE_NAME_BYTES_MAX} of a-z, 0-9 and ._-")));
            }
            let by_arch: BTreeMap<Arch, String> = match refs {
                WireRefs::Every(r) => Arch::ALL.into_iter().map(|a| (a, r.clone())).collect(),
                WireRefs::ByArch(m) => m,
            };
            if by_arch.is_empty() {
                return Err(bad("a reference, or one for at least one architecture".into()));
            }
            if let Some(r) = by_arch.values().find(|r| !valid_reference(r)) {
                return Err(bad(format!("{r:?} is not a reference (1 to {REFERENCE_BYTES_MAX} printable bytes)")));
            }
            images.insert(name, by_arch);
        }
        Ok(Nodes { nodes, images })
    }

    /// The nodes, in the deployment's order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn get(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The image names computers may be pinned to.
    pub fn image_names(&self) -> impl Iterator<Item = &str> {
        self.images.keys().map(String::as_str)
    }

    /// What `image` is on a node of `arch`, if the deployment names one.
    pub fn reference(&self, image: &str, arch: Arch) -> Option<&str> {
        self.images.get(image)?.get(&arch).map(String::as_str)
    }

    /// The image names a node of `arch` can run.
    pub fn images_for(&self, arch: Arch) -> BTreeMap<&str, &str> {
        self.images.iter().filter_map(|(name, refs)| refs.get(&arch).map(|r| (name.as_str(), r.as_str()))).collect()
    }

    /// The checked configuration as the cell's JavaScript reads it
    /// (entry.mjs, node.mjs): each node's reach, its secret's name, and the
    /// images it can run by name.
    pub fn for_js(&self) -> serde_json::Value {
        let nodes: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "url": match &n.reach { Reach::Url(u) => Some(u.as_str()), Reach::Uplink => None },
                    "uplink": n.reach == Reach::Uplink,
                    "arch": n.arch.name(),
                    "capacity": n.capacity,
                    "secret": secret_name(&n.id),
                    "images": self.images_for(n.arch),
                })
            })
            .collect();
        serde_json::json!({ "nodes": nodes, "images": self.image_names().collect::<Vec<_>>() })
    }
}

/// What the platform found of one node as it placed a computer: its
/// health within the probe's deadline, and its object's count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub id: String,
    /// Why it is not up: it did not answer its health in time, or answered
    /// an error. `None`: it is up.
    #[serde(default)]
    pub down: Option<String>,
    /// The architecture its health reports (`None`: a node that does not
    /// say, which is taken at its listing's word).
    #[serde(default)]
    pub arch: Option<String>,
    /// The computers placed on it (its `Node` object's count).
    pub placed: u32,
}

/// Why one node cannot take a computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    /// It was not probed (a node added since the probe began).
    Unprobed,
    Down(String),
    /// It reports another architecture than its listing says.
    Arch { listed: Arch, reported: String },
    /// The deployment names no reference to the image for its architecture.
    NoImage { image: String, arch: Arch },
    Full { placed: u32, capacity: u32 },
}

impl fmt::Display for Unfit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unfit::Unprobed => write!(f, "not probed"),
            Unfit::Down(why) => write!(f, "down ({why})"),
            Unfit::Arch { listed, reported } => write!(f, "listed as {} but reports {reported}", listed.name()),
            Unfit::NoImage { image, arch } => write!(f, "no {image} image for {}", arch.name()),
            Unfit::Full { placed, capacity } => write!(f, "full ({placed} of {capacity} computers)"),
        }
    }
}

/// No node can take the computer: each node with why, in the
/// deployment's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unplaced(pub Vec<(String, Unfit)>);

impl fmt::Display for Unplaced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no node can take it: ")?;
        for (i, (id, why)) in self.0.iter().enumerate() {
            write!(f, "{}{id} is {why}", if i == 0 { "" } else { "; " })?;
        }
        Ok(())
    }
}

/// Why `node`, as `probe` found it, cannot take a computer of `image`.
fn unfit(nodes: &Nodes, node: &Node, image: &str, probe: Option<&Probe>) -> Option<Unfit> {
    let Some(p) = probe else { return Some(Unfit::Unprobed) };
    if let Some(why) = &p.down {
        return Some(Unfit::Down(why.clone()));
    }
    if let Some(reported) = p.arch.as_deref().filter(|r| Arch::parse(r) != Some(node.arch)) {
        return Some(Unfit::Arch { listed: node.arch, reported: reported.to_string() });
    }
    if nodes.reference(image, node.arch).is_none() {
        return Some(Unfit::NoImage { image: image.to_string(), arch: node.arch });
    }
    if p.placed >= node.capacity {
        return Some(Unfit::Full { placed: p.placed, capacity: node.capacity });
    }
    None
}

/// The nodes that can take a new computer of `image`, best first (the
/// module's rule), or why none can.
pub fn rank(nodes: &Nodes, image: &str, probes: &[Probe]) -> Result<Vec<String>, Unplaced> {
    let mut fit: Vec<(usize, &Node, u32)> = vec![];
    let mut why = vec![];
    for (i, node) in nodes.nodes.iter().enumerate() {
        let probe = probes.iter().find(|p| p.id == node.id);
        match unfit(nodes, node, image, probe) {
            Some(u) => why.push((node.id.clone(), u)),
            None => fit.push((i, node, probe.expect("a fit node was probed").placed)),
        }
    }
    if fit.is_empty() {
        assert_eq!(why.len(), nodes.nodes.len(), "every node says why it cannot take it");
        return Err(Unplaced(why));
    }
    // fewest for its capacity: a/b < c/d as a*d < c*b, in u64 (no overflow
    // under CAPACITY_MAX); the stable sort keeps the deployment's order in a tie
    fit.sort_by(|(i, a, pa), (j, b, pb)| (u64::from(*pa) * u64::from(b.capacity)).cmp(&(u64::from(*pb) * u64::from(a.capacity))).then(i.cmp(j)));
    Ok(fit.into_iter().map(|(_, n, _)| n.id.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = r#"{
        "nodes": [
            { "id": "box", "url": "http://192.168.50.7:9400/", "arch": "x86_64", "capacity": 4 },
            { "id": "mac", "uplink": true, "arch": "aarch64", "capacity": 2 }
        ],
        "images": {
            "stub": { "x86_64": "docker.io/library/fragment-stub:3", "aarch64": "docker.io/library/fragment-stub:3-arm64" },
            "hermes": { "x86_64": "registry.example/fragment-hermes:7" },
            "multi": "registry.example/fragment-multi:1"
        }
    }"#;

    fn two() -> Nodes {
        Nodes::parse(TWO).unwrap()
    }

    fn up(id: &str, placed: u32) -> Probe {
        Probe { id: id.into(), down: None, arch: None, placed }
    }

    // Goal: the configuration reads as written; a URL loses its trailing
    // slash, a single reference stands for every architecture.
    #[test]
    fn a_configuration_reads() {
        let n = two();
        assert_eq!(n.nodes().len(), 2);
        assert_eq!(n.get("box").unwrap().reach, Reach::Url("http://192.168.50.7:9400".into()));
        assert_eq!(n.get("mac").unwrap().reach, Reach::Uplink);
        assert_eq!(n.get("mac").unwrap().arch, Arch::Aarch64);
        assert_eq!(n.reference("stub", Arch::Aarch64), Some("docker.io/library/fragment-stub:3-arm64"));
        assert_eq!(n.reference("hermes", Arch::Aarch64), None);
        assert_eq!(n.reference("multi", Arch::Aarch64), n.reference("multi", Arch::X86_64));
        assert_eq!(n.image_names().collect::<Vec<_>>(), ["hermes", "multi", "stub"]);
        assert_eq!(n.images_for(Arch::Aarch64).keys().copied().collect::<Vec<_>>(), ["multi", "stub"]);
        let js = n.for_js();
        assert_eq!(js["nodes"][0]["secret"], "FRAGMENT_NODE_SECRET_BOX");
        assert_eq!(js["nodes"][1]["url"], serde_json::Value::Null);
        assert_eq!(js["nodes"][1]["uplink"], true);
        assert_eq!(js["nodes"][1]["images"]["stub"], "docker.io/library/fragment-stub:3-arm64");
        assert!(js["nodes"][1]["images"].get("hermes").is_none());
    }

    // Goal: every malformed configuration is refused, saying what is wrong.
    #[test]
    fn a_bad_configuration_is_refused() {
        let node = |extra: &str| format!(r#"{{ "nodes": [{extra}], "images": {{ "stub": "s:1" }} }}"#);
        let cases: Vec<(String, &str)> = vec![
            ("[]".into(), "FRAGMENT_NODES is"),
            (r#"{ "nodes": [], "images": { "stub": "s:1" } }"#.into(), "1 to 64 nodes"),
            (node(r#"{ "id": "Box", "url": "http://a", "arch": "x86_64", "capacity": 1 }"#), "not a node's id"),
            (node(r#"{ "id": "a_b", "url": "http://a", "arch": "x86_64", "capacity": 1 }"#), "not a node's id"),
            (node(r#"{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 1 }, { "id": "a", "uplink": true, "arch": "x86_64", "capacity": 1 }"#), "listed twice"),
            (node(r#"{ "id": "a", "url": "http://a", "uplink": true, "arch": "x86_64", "capacity": 1 }"#), "not both"),
            (node(r#"{ "id": "a", "arch": "x86_64", "capacity": 1 }"#), "a url, or"),
            (node(r#"{ "id": "a", "url": "ftp://a", "arch": "x86_64", "capacity": 1 }"#), "http(s) origin"),
            (node(r#"{ "id": "a", "url": "http://a/api", "arch": "x86_64", "capacity": 1 }"#), "http(s) origin"),
            (node(r#"{ "id": "a", "url": "http://u:p@a", "arch": "x86_64", "capacity": 1 }"#), "http(s) origin"),
            (node(r#"{ "id": "a", "url": "http://a", "arch": "riscv64", "capacity": 1 }"#), "FRAGMENT_NODES is"),
            (node(r#"{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 0 }"#), "capacity is 1 to"),
            (node(r#"{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 10001 }"#), "capacity is 1 to"),
            (node(r#"{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 1, "secret": "x" }"#), "FRAGMENT_NODES is"),
            (r#"{ "nodes": [{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 1 }], "images": {} }"#.into(), "1 to 32 images"),
            (r#"{ "nodes": [{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 1 }], "images": { "Stub": "s:1" } }"#.into(), "a name is"),
            (r#"{ "nodes": [{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 1 }], "images": { "stub": "s 1" } }"#.into(), "not a reference"),
            (r#"{ "nodes": [{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 1 }], "images": { "stub": {} } }"#.into(), "at least one architecture"),
            (r#"{ "nodes": [{ "id": "a", "url": "http://a", "arch": "x86_64", "capacity": 1 }], "images": { "stub": { "sparc": "s:1" } } }"#.into(), "FRAGMENT_NODES is"),
        ];
        for (json, says) in cases {
            let e = Nodes::parse(&json).expect_err(&json);
            assert!(e.to_string().contains(says), "{json}: {e} (wanted {says:?})");
        }
        let many: Vec<String> = (0..=NODES_MAX).map(|i| format!(r#"{{ "id": "n{i}", "uplink": true, "arch": "x86_64", "capacity": 1 }}"#)).collect();
        assert_eq!(Nodes::parse(&node(&many.join(","))), Err(NodesError::Count(NODES_MAX + 1)));
    }

    #[test]
    fn a_secret_is_named_by_its_node() {
        assert_eq!(secret_name("box"), "FRAGMENT_NODE_SECRET_BOX");
        assert_eq!(secret_name("office-1"), "FRAGMENT_NODE_SECRET_OFFICE_1");
        assert!(valid_node_id(&"a".repeat(NODE_ID_BYTES_MAX)) && !valid_node_id(&"a".repeat(NODE_ID_BYTES_MAX + 1)) && !valid_node_id(""));
    }

    // Goal: the fewest computers for its capacity wins; a tie goes to the
    // node listed first; computers spread as they are placed one by one.
    #[test]
    fn the_least_loaded_node_takes_it() {
        let n = two();
        assert_eq!(rank(&n, "stub", &[up("box", 0), up("mac", 0)]).unwrap(), ["box", "mac"]);
        assert_eq!(rank(&n, "stub", &[up("box", 1), up("mac", 0)]).unwrap(), ["mac", "box"]);
        // 2 of 4 against 1 of 2: a tie, the deployment's order
        assert_eq!(rank(&n, "stub", &[up("box", 2), up("mac", 1)]).unwrap(), ["box", "mac"]);
        assert_eq!(rank(&n, "stub", &[up("box", 3), up("mac", 1)]).unwrap(), ["mac", "box"]);
        // placed one at a time, each where the rule says: box, mac, box, box, mac, box
        let (mut b, mut m, mut order) = (0, 0, vec![]);
        for _ in 0..6 {
            let first = rank(&n, "stub", &[up("box", b), up("mac", m)]).unwrap()[0].clone();
            if first == "box" { b += 1 } else { m += 1 }
            order.push(first);
        }
        assert_eq!(order, ["box", "mac", "box", "box", "mac", "box"]);
        assert_eq!((b, m), (4, 2));
        // both full
        let e = rank(&n, "stub", &[up("box", 4), up("mac", 2)]).unwrap_err();
        assert_eq!(e.to_string(), "no node can take it: box is full (4 of 4 computers); mac is full (2 of 2 computers)");
    }

    // Goal: a node that is down, that reports another architecture, or
    // that has no reference to the image for its own, takes nothing; one
    // not probed is down.
    #[test]
    fn a_node_that_cannot_run_it_takes_nothing() {
        let n = two();
        let down = Probe { down: Some("connection refused".into()), ..up("box", 0) };
        assert_eq!(rank(&n, "stub", &[down.clone(), up("mac", 1)]).unwrap(), ["mac"]);
        let wrong = Probe { arch: Some("x86_64".into()), ..up("mac", 0) };
        assert_eq!(rank(&n, "stub", &[up("box", 3), wrong.clone()]).unwrap(), ["box"]);
        let right = Probe { arch: Some("aarch64".into()), ..up("mac", 0) };
        assert_eq!(rank(&n, "stub", &[up("box", 3), right]).unwrap(), ["mac", "box"]);
        assert_eq!(rank(&n, "hermes", &[up("box", 3), up("mac", 0)]).unwrap(), ["box"]);
        assert_eq!(rank(&n, "stub", &[up("mac", 0)]).unwrap(), ["mac"]);
        let e = rank(&n, "hermes", &[down, wrong]).unwrap_err();
        assert_eq!(e.0, vec![("box".to_string(), Unfit::Down("connection refused".into())), ("mac".to_string(), Unfit::Arch { listed: Arch::Aarch64, reported: "x86_64".into() })]);
        assert_eq!(e.to_string(), "no node can take it: box is down (connection refused); mac is listed as aarch64 but reports x86_64");
        let e = rank(&n, "hermes", &[up("box", 4)]).unwrap_err();
        assert_eq!(e.to_string(), "no node can take it: box is full (4 of 4 computers); mac is not probed");
        assert_eq!(rank(&n, "nonesuch", &[up("box", 0), up("mac", 0)]).unwrap_err().0.len(), 2);
    }

    // Goal: probes arrive as JSON from the cell's JavaScript; a replayed
    // probe (the same answers) ranks the same.
    #[test]
    fn probes_read_from_json_and_rank_the_same_again() {
        let probes: Vec<Probe> = serde_json::from_str(r#"[{"id":"box","placed":1},{"id":"mac","down":"no uplink open","placed":0}]"#).unwrap();
        let n = two();
        let first = rank(&n, "stub", &probes).unwrap();
        assert_eq!(first, ["box"]);
        assert_eq!(rank(&n, "stub", &probes).unwrap(), first);
    }
}
