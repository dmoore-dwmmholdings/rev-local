//! Manual capability overrides (RL-605, SPEC §11.2).
//!
//! §11.2 says the UI "offers a manual override (pick tool + map fields)" when a
//! capability will not bind. This is where that choice is kept.
//!
//! # Overrides are a user choice, and they live apart from the user's config
//!
//! ADR 0015 draws the line between what the user chose and what rev-local found,
//! and keeps them in different places so a report can tell you which is which. An
//! override is squarely a user choice — but it is one *rev-local writes*, from
//! `revlocal targets map`, and rewriting a hand-maintained `config.toml` would
//! either lose the user's comments and formatting or require a round-tripping TOML
//! editor to avoid it.
//!
//! So overrides live in their own file, owned by rev-local. That also keeps the
//! distinction §11.2 needs to display: this capability bound because you *said* so,
//! not because resolution found it.
//!
//! # What can be checked when an override is saved
//!
//! The criterion is that an override is validated "at save time, not at first
//! use", and it is worth being precise about what that can mean. At save time
//! there is no finding to render, so no argument *values* exist yet. What does
//! exist is the tool's schema, and two things are checkable against it:
//!
//!   * the named tool exists on the server at all;
//!   * every property the schema marks `required` is supplied by the template.
//!
//! Those are exactly the mistakes that would otherwise wait until a real
//! publish — a typo'd tool name, or a template missing a mandatory field. Value
//! validation still happens at render, because that is when values exist.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability::{Binding, TargetMapping, Unmapped};
use crate::protocol::Tool;

/// One manual binding, as `revlocal targets map` recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    /// Which target.
    pub target: String,
    /// Which capability.
    pub capability: String,
    /// The tool the user picked.
    pub tool: String,
    /// The argument template the user supplied.
    pub args: Value,
}

impl Override {
    /// Check this override against the tool the server reported.
    ///
    /// See the module docs for what this can and cannot prove.
    pub fn check_against(&self, tools: &[Tool]) -> Result<(), OverrideError> {
        let tool = tools.iter().find(|t| t.name == self.tool).ok_or_else(|| {
            OverrideError::NoSuchTool {
                tool: self.tool.clone(),
                available: tools.iter().map(|t| t.name.clone()).collect(),
            }
        })?;

        let required: Vec<String> = tool
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let missing: Vec<String> = required
            .into_iter()
            .filter(|field| self.args.get(field).is_none())
            .collect();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(OverrideError::MissingRequired {
                tool: self.tool.clone(),
                capability: self.capability.clone(),
                fields: missing,
            })
        }
    }
}

/// Why an override could not be saved or loaded.
#[derive(Debug, thiserror::Error)]
pub enum OverrideError {
    /// The server does not expose the tool the user named.
    #[error("`{tool}` is not a tool this server has; it has [{}]\n  try: pick one of those names", available.join(", "))]
    NoSuchTool {
        /// What was named.
        tool: String,
        /// What the server actually has.
        available: Vec<String>,
    },

    /// The template omits a field the tool requires.
    #[error("`{tool}` requires [{}], which capability `{capability}`'s override does not set\n  try: add --arg for each of them", fields.join(", "))]
    MissingRequired {
        /// Which tool.
        tool: String,
        /// Which capability.
        capability: String,
        /// The fields that are missing.
        fields: Vec<String>,
    },

    /// The overrides file could not be read or written.
    #[error("could not {action} {path}: {source}")]
    Io {
        /// `read` or `write`.
        action: &'static str,
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },

    /// The overrides file is not readable as JSON.
    #[error("{path} is not a readable overrides file: {source}\n  try: delete it and re-run `revlocal targets map`")]
    Malformed {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

/// Every saved override.
///
/// Keyed by `(target, capability)`: one capability has at most one override, and
/// saving a second replaces the first rather than accumulating rules whose
/// precedence somebody would then have to reason about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Overrides {
    entries: BTreeMap<String, Override>,
}

fn key(target: &str, capability: &str) -> String {
    format!("{target}/{capability}")
}

impl Overrides {
    /// No overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the overrides file.
    ///
    /// A file that does not exist is not an error: it is the normal state of a
    /// system nobody has had to override anything on.
    pub fn load(path: &Path) -> Result<Self, OverrideError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(source) => {
                return Err(OverrideError::Io {
                    action: "read",
                    path: path.display().to_string(),
                    source,
                })
            }
        };

        serde_json::from_str(&text).map_err(|source| OverrideError::Malformed {
            path: path.display().to_string(),
            source,
        })
    }

    /// Write the overrides file, creating its directory if needed.
    pub fn save(&self, path: &Path) -> Result<(), OverrideError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| OverrideError::Io {
                action: "create the directory for",
                path: path.display().to_string(),
                source,
            })?;
        }

        // Pretty, and with a trailing newline: this is a file a user may open to
        // see what they told rev-local to do.
        let text = serde_json::to_string_pretty(self).map_err(|e| OverrideError::Io {
            action: "serialize",
            path: path.display().to_string(),
            source: std::io::Error::other(e),
        })?;

        std::fs::write(path, format!("{text}\n")).map_err(|source| OverrideError::Io {
            action: "write",
            path: path.display().to_string(),
            source,
        })
    }

    /// Record an override, replacing any earlier one for the same capability.
    pub fn set(&mut self, entry: Override) {
        self.entries
            .insert(key(&entry.target, &entry.capability), entry);
    }

    /// The override for one capability, if there is one.
    pub fn get(&self, target: &str, capability: &str) -> Option<&Override> {
        self.entries.get(&key(target, capability))
    }

    /// Remove one override. `true` if there was one.
    pub fn clear(&mut self, target: &str, capability: &str) -> bool {
        self.entries.remove(&key(target, capability)).is_some()
    }

    /// Every override for one target, in capability order.
    pub fn for_target<'a>(&'a self, target: &'a str) -> impl Iterator<Item = &'a Override> {
        self.entries.values().filter(move |o| o.target == target)
    }

    /// How many overrides are saved.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is overridden.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Apply this target's overrides to a resolved mapping.
    ///
    /// An override wins over resolution — that is what it is for — and an override
    /// naming a tool the server does not have leaves the capability unmapped
    /// rather than binding to nothing. The tool may have gone away since the
    /// override was saved; save-time validation cannot prevent that, only report
    /// it when it happens.
    pub fn apply(&self, mapping: &mut TargetMapping, tools: &[Tool]) {
        let available: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

        for entry in self.for_target(&mapping.target) {
            mapping.bound.retain(|b| b.capability != entry.capability);
            mapping
                .unmapped
                .retain(|u| u.capability != entry.capability);

            match tools.iter().find(|t| t.name == entry.tool) {
                Some(tool) => mapping.bound.push(Binding {
                    capability: entry.capability.clone(),
                    tool: tool.name.clone(),
                    candidate_index: 0,
                    schema: tool.input_schema.clone(),
                    args: entry.args.clone(),
                    from_override: true,
                }),
                None => mapping.unmapped.push(Unmapped {
                    capability: entry.capability.clone(),
                    candidates: vec![entry.tool.clone()],
                    available: available.clone(),
                }),
            }
        }

        mapping
            .bound
            .sort_by(|a, b| a.capability.cmp(&b.capability));
        mapping
            .unmapped
            .sort_by(|a, b| a.capability.cmp(&b.capability));
    }
}

/// Parse a `key=value` argument as `revlocal targets map --arg` takes it.
///
/// Values are templates, so they stay strings here — `{finding.title}` means
/// nothing until it is rendered. A value that parses as JSON is taken as JSON, so
/// `--arg points=3` and `--arg labels=["a","b"]` express a number and an array
/// rather than strings that look like them.
pub fn parse_arg(text: &str) -> Option<(String, Value)> {
    let (name, value) = text.split_once('=')?;
    if name.is_empty() {
        return None;
    }

    let parsed = serde_json::from_str::<Value>(value)
        .ok()
        .filter(|v| !v.is_string())
        .unwrap_or_else(|| Value::String(value.to_owned()));

    Some((name.to_owned(), parsed))
}
