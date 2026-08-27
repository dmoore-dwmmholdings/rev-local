//! Acceptance test for `RL-104` — `revlocal-core` has no I/O dependencies.
//!
//! SPEC §4.1: "`revlocal-core` has no I/O dependencies (no tokio, no sqlx, no
//! reqwest). Every other crate may depend on it. This keeps the domain model
//! unit-testable."
//!
//! The check is on the *transitive* closure, not the manifest, because that is
//! where this rule actually breaks: nobody adds tokio to `revlocal-core` on
//! purpose. It arrives three hops down, inside something that looked pure. So the
//! test walks `cargo metadata`'s resolve graph and reports the whole path, since
//! knowing that tokio is in the tree is useless without knowing who pulled it in.
//!
//! Stated as an **exclusion, not an allowlist** (ADR 0005): adding a genuinely
//! pure dependency must not require editing this test, or the test becomes
//! something people edit reflexively rather than read.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::process::Command;

/// The crate under the rule.
const ROOT_CRATE: &str = "revlocal-core";

/// Crates that mean `revlocal-core` has stopped being a pure domain model.
///
/// An async runtime, a database driver, or an HTTP stack in this tree each mean
/// the domain can no longer be constructed and asserted on without one.
const BANNED: &[&str] = &["tokio", "sqlx", "reqwest", "hyper", "rusqlite"];

/// One edge of the resolve graph, already resolved to crate names.
struct Edge {
    to: String,
    kind: &'static str,
}

/// Parse `cargo metadata` into a name-keyed adjacency list.
///
/// Returns `Err` rather than unwrapping: this is a helper, not a `#[test]` fn, so
/// clippy's unwrap/expect ban applies (ADR 0003).
fn dependency_graph() -> Result<BTreeMap<String, Vec<Edge>>, String> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .output()
        .map_err(|e| format!("running `cargo metadata`: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("parsing metadata: {e}"))?;

    // Package id -> crate name. Ids are opaque and their format has changed
    // between cargo versions, so nothing here parses them.
    let mut name_of: HashMap<&str, &str> = HashMap::new();
    let packages = metadata["packages"]
        .as_array()
        .ok_or("metadata has no `packages` array")?;
    for package in packages {
        let (Some(id), Some(name)) = (package["id"].as_str(), package["name"].as_str()) else {
            return Err("a package entry is missing `id` or `name`".to_owned());
        };
        name_of.insert(id, name);
    }

    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("metadata has no `resolve.nodes`; was --no-deps passed?")?;

    let mut graph: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
    for node in nodes {
        let id = node["id"].as_str().ok_or("a resolve node has no `id`")?;
        let from = (*name_of.get(id).ok_or("resolve node is not in `packages`")?).to_owned();
        let entry = graph.entry(from).or_default();

        for dep in node["deps"].as_array().into_iter().flatten() {
            let pkg = dep["pkg"].as_str().ok_or("a dep has no `pkg`")?;
            let to = (*name_of.get(pkg).ok_or("dep package is not in `packages`")?).to_owned();

            // `kind` is absent/null for a normal dependency, "dev" or "build"
            // otherwise. Dev edges are walked too: a unit test in the domain crate
            // that needs an async runtime means the domain went async.
            let kind = dep["dep_kinds"]
                .as_array()
                .and_then(|kinds| kinds.first())
                .and_then(|k| k["kind"].as_str())
                .unwrap_or("normal");
            let kind = match kind {
                "dev" => "dev",
                "build" => "build",
                _ => "normal",
            };
            entry.push(Edge { to, kind });
        }
    }
    Ok(graph)
}

/// Breadth-first search from `ROOT_CRATE` for `banned`, returning the path that
/// reaches it, rendered for a human.
///
/// Breadth-first so the reported path is the *shortest* one — the most direct
/// culprit, rather than whichever route the walk happened to take first.
fn path_to(graph: &BTreeMap<String, Vec<Edge>>, banned: &str) -> Option<String> {
    let mut seen: HashSet<&str> = HashSet::from([ROOT_CRATE]);
    let mut queue: VecDeque<&str> = VecDeque::from([ROOT_CRATE]);
    let mut came_from: HashMap<&str, (&str, &str)> = HashMap::new();

    while let Some(current) = queue.pop_front() {
        for edge in graph.get(current).into_iter().flatten() {
            if edge.to == banned {
                let mut hops = vec![format!("{} ({})", banned, edge.kind)];
                let mut cursor = current;
                while let Some((parent, kind)) = came_from.get(cursor) {
                    hops.push(format!("{cursor} ({kind})"));
                    cursor = parent;
                }
                hops.push(ROOT_CRATE.to_owned());
                hops.reverse();
                return Some(hops.join(" -> "));
            }
            if seen.insert(&edge.to) {
                came_from.insert(&edge.to, (current, edge.kind));
                queue.push_back(&edge.to);
            }
        }
    }
    None
}

#[test]
fn revlocal_core_has_no_io_dependencies() {
    let graph = dependency_graph().unwrap_or_else(|e| panic!("{e}"));
    assert!(
        graph.contains_key(ROOT_CRATE),
        "{ROOT_CRATE} is not in the resolve graph; the test is looking at the wrong workspace"
    );

    let violations: Vec<String> = BANNED
        .iter()
        .filter_map(|banned| path_to(&graph, banned))
        .collect();

    assert!(
        violations.is_empty(),
        "SPEC §4.1: {ROOT_CRATE} must have no I/O dependencies, but its dependency \
         tree reaches {} of them:\n  {}\n\nIf the domain genuinely needs this, it \
         belongs in another crate — see SPEC §4.1 and ADR 0005.",
        violations.len(),
        violations.join("\n  ")
    );
}

#[test]
fn the_check_would_actually_catch_a_violation() {
    // A test that can only pass is not a test. This asserts the search works by
    // pointing it at a crate that IS in the tree: if `path_to` cannot find serde,
    // which revlocal-core depends on directly, then it would not find tokio either
    // and the guarantee above is decorative.
    let graph = dependency_graph().unwrap_or_else(|e| panic!("{e}"));

    let found = path_to(&graph, "serde").unwrap_or_else(|| {
        panic!("path_to found no route to serde, which {ROOT_CRATE} depends on directly")
    });
    assert!(
        found.starts_with(ROOT_CRATE),
        "a reported path must start at {ROOT_CRATE}, got {found:?}"
    );
    assert!(
        found.contains("serde"),
        "a reported path must name the offending crate, got {found:?}"
    );

    // ...and it must not invent a path to something absent from the tree.
    assert!(
        path_to(&graph, "definitely-not-a-real-crate").is_none(),
        "the search must not report a path to a crate that is not there"
    );
}
