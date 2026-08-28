//! Capability mapping (RL-604, SPEC §11.2).
//!
//! §11.2's claim is that integrating Andare does not require knowing Andare's tool
//! names at build time. This module is what makes that true: config names a list
//! of *candidates* per capability, and the first one the server actually exposes
//! wins.
//!
//! Three rules do the work, and each exists because the obvious alternative fails
//! quietly rather than loudly.
//!
//! # Candidates match exactly, or not at all
//!
//! No fuzzy matching, no prefix matching, no "closest name". A server exposing
//! `create_ticket` when config asked for `create_issue`, `create_work_item`,
//! `issue_create` is **unmapped** — and unmapped is reported, never guessed. The
//! failure mode of a near-match is filing a finding into whatever `create_thing`
//! happened to be, with arguments it interprets differently, and discovering it
//! when somebody reads the wrong project's issue tracker a week later.
//!
//! # Arguments are rendered from a template and then *validated*
//!
//! Rendering can produce a payload the tool will reject — a missing required
//! field, a summary past the server's `maxLength`. Sending it and reading the
//! error back is one round trip and one confusing message; validating against the
//! tool's own `inputSchema` first is neither. The schema comes from the server, so
//! this is the server's own definition of a valid call, not rev-local's guess at
//! one.
//!
//! # A placeholder that resolves to nothing is an error
//!
//! Not an empty string. §18's no-silent-caps rule applied to arguments: an issue
//! filed with an empty title is worse than an issue not filed, because it looks
//! like it worked.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::protocol::Tool;

/// One capability's binding rule, as written in `[targets.<t>.map.<capability>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySpec {
    /// What rev-local wants to do — `create_issue`, `set_status`, `upsert_page`.
    pub name: String,
    /// Tool names to try, in order. First match on the server wins.
    pub tool_candidates: Vec<String>,
    /// The argument template, rendered per call.
    pub args: Value,
}

/// One publish target's capability map (`[targets.<t>]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSpec {
    /// Target id — `andare`, `trama`, `github`.
    pub id: String,
    /// Which configured MCP server it speaks to.
    pub mcp_server: String,
    /// Capabilities, in config order.
    pub capabilities: Vec<CapabilitySpec>,
}

impl TargetSpec {
    /// Read a `[targets.<id>]` table.
    ///
    /// The table is `toml::Value` because §13.1 deliberately keeps target shapes
    /// opaque to the config loader — this module owns the shape, so this is where
    /// it is parsed and where a bad one is reported.
    pub fn from_toml(id: &str, table: &toml::Value) -> Result<Self, SpecError> {
        let mcp_server = table
            .get("mcp_server")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| SpecError::MissingField {
                target: id.to_owned(),
                field: "mcp_server".to_owned(),
            })?
            .to_owned();

        let mut capabilities = Vec::new();
        if let Some(map) = table.get("map").and_then(toml::Value::as_table) {
            for (name, entry) in map {
                let candidates = entry
                    .get("tool_candidates")
                    .and_then(toml::Value::as_array)
                    .ok_or_else(|| SpecError::MissingField {
                        target: id.to_owned(),
                        field: format!("map.{name}.tool_candidates"),
                    })?
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect::<Vec<_>>();

                if candidates.is_empty() {
                    return Err(SpecError::NoCandidates {
                        target: id.to_owned(),
                        capability: name.clone(),
                    });
                }

                let args =
                    entry
                        .get("args")
                        .map_or(Ok(Value::Object(serde_json::Map::new())), |a| {
                            toml_to_json(a).ok_or_else(|| SpecError::BadArgs {
                                target: id.to_owned(),
                                capability: name.clone(),
                            })
                        })?;

                capabilities.push(CapabilitySpec {
                    name: name.clone(),
                    tool_candidates: candidates,
                    args,
                });
            }
        }

        Ok(Self {
            id: id.to_owned(),
            mcp_server,
            capabilities,
        })
    }
}

/// Built-in candidate lists for targets rev-local knows about (§11.2's "built-in
/// profile per known target").
///
/// The names come from a live server's `tools/list`, recorded in ADR 0028, and the
/// real name is listed first because candidate order is priority order. The
/// alternatives after it are what a differently-named server might call the same
/// operation — which is the whole point: a built-in profile is a starting guess,
/// not a requirement.
///
/// `None` for a target with no built-in profile; the user's config supplies it.
pub fn builtin_target(id: &str) -> Option<TargetSpec> {
    let toml_text = match id {
        "andare" => ANDARE_PROFILE,
        _ => return None,
    };

    // Parsed through the same path as user config, so a malformed built-in fails
    // the same way a malformed config would — and the test that parses these is
    // testing what production parses.
    let table: toml::Value = toml_text.parse().ok()?;
    TargetSpec::from_toml(id, &table).ok()
}

/// Andare's profile. Argument names are Andare's own: `summary` not `title`,
/// `description` not `body` (ADR 0028).
const ANDARE_PROFILE: &str = r#"
mcp_server = "andare"

[map.create_issue]
tool_candidates = ["create_issue", "create_work_item", "issue_create", "create_ticket"]
args = { project = "{repo.config.andare_project}", summary = "{finding.title}", description = "{finding.body_md}" }

[map.set_status]
tool_candidates = ["set_issue_status", "update_issue", "transition_issue"]
args = { key = "{issue_ref}", status = "{status}" }

[map.comment]
tool_candidates = ["comment_on_issue", "add_comment", "create_comment"]
args = { key = "{issue_ref}", body = "{comment.body_md}" }

[map.search]
tool_candidates = ["search_issues", "search", "find_issues"]
args = { project = "{repo.config.andare_project}", aql = "{query}" }
"#;

/// A `[targets.*]` table this module could not read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpecError {
    /// A required key is absent.
    #[error("target `{target}` is missing `{field}`\n  try: add it under [targets.{target}] in your config")]
    MissingField {
        /// Which target.
        target: String,
        /// Which key.
        field: String,
    },

    /// `tool_candidates` was present but empty.
    #[error("target `{target}` capability `{capability}` lists no tool candidates\n  try: name at least one tool the server may expose")]
    NoCandidates {
        /// Which target.
        target: String,
        /// Which capability.
        capability: String,
    },

    /// The `args` table could not be represented as JSON.
    #[error("target `{target}` capability `{capability}` has arguments that are not representable as JSON\n  try: use strings, numbers, booleans, arrays and tables only")]
    BadArgs {
        /// Which target.
        target: String,
        /// Which capability.
        capability: String,
    },
}

/// TOML to JSON, for argument templates.
///
/// Datetimes are the one TOML type with no JSON equivalent, and they are rejected
/// rather than stringified: a tool expecting a string timestamp should be given a
/// string in config, so the shape reaching the server is the shape that was
/// written.
fn toml_to_json(value: &toml::Value) -> Option<Value> {
    Some(match value {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map(Value::Number)?,
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(_) => return None,
        toml::Value::Array(items) => {
            Value::Array(items.iter().map(toml_to_json).collect::<Option<Vec<_>>>()?)
        }
        toml::Value::Table(table) => Value::Object(
            table
                .iter()
                .map(|(k, v)| toml_to_json(v).map(|v| (k.clone(), v)))
                .collect::<Option<serde_json::Map<_, _>>>()?,
        ),
    })
}

/// A capability bound to a tool the server actually has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// What rev-local wanted to do.
    pub capability: String,
    /// The tool it resolved to.
    pub tool: String,
    /// Which candidate position matched, for the UI's "resolved to" line.
    pub candidate_index: usize,
    /// The tool's own input schema, as the server reported it.
    pub schema: Value,
    /// The argument template.
    pub args: Value,
    /// Whether this binding came from a manual override rather than from
    /// resolution (RL-605).
    ///
    /// §11.2 needs the two to be distinguishable in the UI, and ADR 0015's rule
    /// applies: "you told us to" and "we worked it out" are different answers to
    /// the question a report exists to answer.
    pub from_override: bool,
}

/// Why a capability could not be bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmapped {
    /// What rev-local wanted to do.
    pub capability: String,
    /// What was looked for.
    pub candidates: Vec<String>,
    /// What the server had instead — so the UI can offer a manual override
    /// (RL-605) without asking the server again.
    pub available: Vec<String>,
}

impl Unmapped {
    /// A line a human can act on.
    pub fn explain(&self) -> String {
        format!(
            "`{}` is unmapped: none of [{}] is exposed by the server, which has [{}]",
            self.capability,
            self.candidates.join(", "),
            self.available.join(", ")
        )
    }
}

/// One target resolved against one server's discovered tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetMapping {
    /// Which target.
    pub target: String,
    /// Which server it was resolved against.
    pub server: String,
    /// Capabilities that bound.
    pub bound: Vec<Binding>,
    /// Capabilities that did not.
    pub unmapped: Vec<Unmapped>,
}

impl TargetMapping {
    /// The binding for one capability, if it bound.
    pub fn binding(&self, capability: &str) -> Option<&Binding> {
        self.bound.iter().find(|b| b.capability == capability)
    }

    /// Whether every capability bound.
    pub fn is_complete(&self) -> bool {
        self.unmapped.is_empty()
    }

    /// The line `revlocal targets list` prints.
    pub fn summary_line(&self) -> String {
        format!(
            "{} → {}: {} mapped, {} unmapped",
            self.target,
            self.server,
            self.bound.len(),
            self.unmapped.len()
        )
    }
}

/// Resolve a target's capabilities against the tools a server reported.
///
/// Pure: it takes the discovered list rather than a client, so resolution is
/// testable without a server and re-runnable from the cache without a round trip.
pub fn resolve(spec: &TargetSpec, tools: &[Tool]) -> TargetMapping {
    let available: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    let by_name: BTreeMap<&str, &Tool> = tools.iter().map(|t| (t.name.as_str(), t)).collect();

    let mut bound = Vec::new();
    let mut unmapped = Vec::new();

    for capability in &spec.capabilities {
        let matched = capability
            .tool_candidates
            .iter()
            .enumerate()
            .find_map(|(index, name)| by_name.get(name.as_str()).map(|tool| (index, *tool)));

        match matched {
            Some((candidate_index, tool)) => bound.push(Binding {
                capability: capability.name.clone(),
                tool: tool.name.clone(),
                candidate_index,
                schema: tool.input_schema.clone(),
                args: capability.args.clone(),
                from_override: false,
            }),
            None => unmapped.push(Unmapped {
                capability: capability.name.clone(),
                candidates: capability.tool_candidates.clone(),
                available: available.clone(),
            }),
        }
    }

    TargetMapping {
        target: spec.id.clone(),
        server: spec.mcp_server.clone(),
        bound,
        unmapped,
    }
}

/// The values `{placeholders}` are resolved against.
///
/// A JSON object, addressed with dotted paths — `{finding.title}`,
/// `{repo.config.andare_project}` — because that is the shape the pipeline already
/// has its finding and repo in, and inventing a second one would mean keeping two
/// in step.
#[derive(Debug, Clone, Default)]
pub struct RenderContext {
    root: Value,
}

impl RenderContext {
    /// A context over one JSON object.
    pub fn new(root: Value) -> Self {
        Self { root }
    }

    /// The value at a dotted path, if there is one.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut cursor = &self.root;
        for segment in path.split('.') {
            cursor = cursor.get(segment)?;
        }
        Some(cursor)
    }
}

/// A rendered payload that was rejected, or could not be built.
#[derive(Debug, thiserror::Error)]
pub enum MappingError {
    /// A placeholder named something the context does not have.
    #[error("capability `{capability}` refers to `{{{placeholder}}}`, which has no value here\n  try: check the argument template for this capability in your config")]
    UnknownPlaceholder {
        /// Which capability.
        capability: String,
        /// The dotted path that did not resolve.
        placeholder: String,
    },

    /// The server's own schema rejected the rendered arguments.
    ///
    /// Carries the violations verbatim, each naming the field it is about, because
    /// "invalid arguments" without a field name sends the reader to the wrong
    /// place in their config.
    #[error("capability `{capability}` would send `{tool}` arguments it rejects: {}\n  try: fix the argument template for this capability", violations.join("; "))]
    SchemaRejected {
        /// Which capability.
        capability: String,
        /// Which tool would have been called.
        tool: String,
        /// One message per violation, each naming the offending field.
        violations: Vec<String>,
    },

    /// The server reported a schema this client cannot compile.
    #[error("tool `{tool}` reported an input schema that could not be read: {detail}")]
    BadSchema {
        /// Which tool.
        tool: String,
        /// What was wrong.
        detail: String,
    },
}

impl Binding {
    /// Render the argument template, then validate it against the tool's schema.
    ///
    /// Both, always, in that order. Rendering alone produces something that looks
    /// like a call; the validation is what makes it one the server will accept.
    pub fn render(&self, context: &RenderContext) -> Result<Value, MappingError> {
        let rendered = self.substitute(&self.args, context)?;
        self.validate(&rendered)?;
        Ok(rendered)
    }

    /// Render without validating.
    ///
    /// Exposed for the UI's "show me what this would send" affordance, which wants
    /// to display a payload even when it is not yet valid.
    pub fn render_unchecked(&self, context: &RenderContext) -> Result<Value, MappingError> {
        self.substitute(&self.args, context)
    }

    /// Check a payload against the tool's own input schema.
    pub fn validate(&self, payload: &Value) -> Result<(), MappingError> {
        let validator =
            jsonschema::validator_for(&self.schema).map_err(|e| MappingError::BadSchema {
                tool: self.tool.clone(),
                detail: e.to_string(),
            })?;

        let violations: Vec<String> = validator
            .iter_errors(payload)
            .map(|e| {
                let path = e.instance_path.to_string();
                if path.is_empty() {
                    e.to_string()
                } else {
                    format!("{e} at {path}")
                }
            })
            .collect();

        if violations.is_empty() {
            Ok(())
        } else {
            Err(MappingError::SchemaRejected {
                capability: self.capability.clone(),
                tool: self.tool.clone(),
                violations,
            })
        }
    }

    /// Walk the template, replacing placeholders.
    fn substitute(&self, template: &Value, context: &RenderContext) -> Result<Value, MappingError> {
        Ok(match template {
            Value::String(text) => self.substitute_string(text, context)?,
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.substitute(item, context))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| self.substitute(v, context).map(|v| (k.clone(), v)))
                    .collect::<Result<serde_json::Map<_, _>, _>>()?,
            ),
            other => other.clone(),
        })
    }

    /// One string.
    ///
    /// A string that is *exactly* one placeholder keeps the referenced value's
    /// type: `{finding.line}` renders as the number 42, not `"42"`. A schema that
    /// says `"type": "integer"` would reject the string, and quietly stringifying
    /// every substitution would make numeric arguments impossible to express.
    fn substitute_string(
        &self,
        text: &str,
        context: &RenderContext,
    ) -> Result<Value, MappingError> {
        if let Some(path) = whole_placeholder(text) {
            return context
                .get(path)
                .cloned()
                .ok_or_else(|| MappingError::UnknownPlaceholder {
                    capability: self.capability.clone(),
                    placeholder: path.to_owned(),
                });
        }

        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}').map(|i| open + i) else {
                break;
            };
            let path = &rest[open + 1..close];
            let value = context
                .get(path)
                .ok_or_else(|| MappingError::UnknownPlaceholder {
                    capability: self.capability.clone(),
                    placeholder: path.to_owned(),
                })?;

            out.push_str(&rest[..open]);
            match value {
                Value::String(s) => out.push_str(s),
                other => out.push_str(&other.to_string()),
            }
            rest = &rest[close + 1..];
        }
        out.push_str(rest);

        Ok(Value::String(out))
    }
}

/// The path in a string that is nothing but one placeholder.
fn whole_placeholder(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('{')?.strip_suffix('}')?;
    if inner.contains('{') || inner.contains('}') || inner.is_empty() {
        None
    } else {
        Some(inner)
    }
}
