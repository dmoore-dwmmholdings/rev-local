//! The domain enums of SPEC §3, §5 and §10–§12.
//!
//! Each is declared with [`string_enum!`], which fixes the wire spelling as an
//! explicit literal. Those literals are the same strings that appear in the
//! SQLite `CHECK` constraints in SPEC §5, so the Rust source is readable as the
//! authority for what a column may contain.

string_enum! {
    /// Which version-control system backs a repository (`repo.kind`, SPEC §6).
    pub enum RepoKind {
        /// A local git working copy or clone.
        Git => "git",
        /// A GitHub repository, reviewed at pull-request granularity.
        GitHub => "github",
        /// A Subversion repository (SPEC §6.4, decision D6).
        Svn => "svn",
    }
}

string_enum! {
    /// Which local AI CLI performs the review (`repo.engine`, decision D3).
    pub enum EngineKind {
        /// Claude Code.
        Claude => "claude",
        /// Codex CLI.
        Codex => "codex",
        /// The offline fixture engine. Never spends tokens; used by the inner loop.
        Mock => "mock",
    }
}

string_enum! {
    /// How much a run may do without a human (SPEC §12.2, decision D4).
    ///
    /// Variants are declared least- to most-permissive, so the derived `Ord` is the
    /// ordering `off < dry_run < auto_low_ask_high < auto` that §12.2 names, and the
    /// effective mode is literally `global.min(repo)`. See [`AutonomyMode::effective`].
    pub enum AutonomyMode {
        /// Reviews do not run.
        Off => "off",
        /// Reviews run; every publish is recorded as `skipped_dry_run`.
        DryRun => "dry_run",
        /// Low-risk publishes are sent; high-risk ones queue for approval.
        AutoLowAskHigh => "auto_low_ask_high",
        /// Everything is sent.
        Auto => "auto",
    }
}

string_enum! {
    /// The atomic unit under review (`change.kind`, SPEC §3).
    pub enum ChangeKind {
        /// A single git commit.
        Commit => "commit",
        /// A GitHub pull request at a specific head SHA.
        Pr => "pr",
        /// One Subversion revision.
        SvnRev => "svn_rev",
        /// A synthesized branch-level merge diff (decision D6).
        SvnPseudoPr => "svn_pseudo_pr",
    }
}

string_enum! {
    /// Lifecycle of one review run (`run.status`, SPEC §5).
    ///
    /// Declaration order is the normal forward path through the pipeline, followed
    /// by the terminal states. It is not a total order over legal transitions —
    /// `RL-109` owns that.
    pub enum RunStatus {
        /// Accepted, waiting for a slot in the run queue.
        Queued => "queued",
        /// Materialising a scratch worktree or export.
        Preparing => "preparing",
        /// The engine process is running.
        Reviewing => "reviewing",
        /// Normalizing and deduping findings.
        Synthesizing => "synthesizing",
        /// Executing publish actions.
        Publishing => "publishing",
        /// At least one high-risk action is queued in the approvals inbox.
        AwaitingApproval => "awaiting_approval",
        /// Finished successfully.
        Done => "done",
        /// Finished with an error; `run.error` says what.
        Failed => "failed",
        /// Not reviewed; `run.skip_reason` says why (SPEC §9.4).
        Skipped => "skipped",
        /// Cancelled by the kill switch or by a user (SPEC §12.1).
        Cancelled => "cancelled",
    }
}

string_enum! {
    /// How thoroughly a change is reviewed (`run.depth`, SPEC §9.3).
    ///
    /// Declared shallowest-first, so the derived `Ord` means "greater is deeper".
    pub enum Depth {
        /// Oversized or doc-only change: 3 minutes, no verification pass.
        Summary => "summary",
        /// The default: 10 minutes.
        Standard => "standard",
        /// 25 minutes, and the engine must try to refute each finding first.
        Deep => "deep",
    }
}

string_enum! {
    /// What caused a run to be created (`run.trigger`, SPEC §7, decision D2).
    pub enum TriggerSource {
        /// Interval polling.
        Poll => "poll",
        /// A local git hook.
        Hook => "hook",
        /// A GitHub webhook delivered over a tunnel.
        Webhook => "webhook",
        /// A human asked for it.
        Manual => "manual",
        /// Backfill over history.
        Backfill => "backfill",
        /// A retry of an earlier attempt.
        Retry => "retry",
    }
}

string_enum! {
    /// How bad a finding is (`finding.severity`, SPEC §10.1).
    ///
    /// Declared least- to most-severe, so the derived `Ord` means "greater is
    /// worse" and `findings.iter().map(|f| f.severity).max()` is the run's worst
    /// finding. Note this is the reverse of the order the §10.1 table prints.
    pub enum Severity {
        /// An observation with no action implied. Summary body only.
        Info => "info",
        /// Minor correctness or convention issue. Inline comment only.
        Low => "low",
        /// A real defect with a narrow blast radius.
        Medium => "medium",
        /// Wrong behaviour a user will hit, or a serious vulnerability.
        High => "high",
        /// Data loss, RCE, auth bypass, corruption.
        Critical => "critical",
    }
}

string_enum! {
    /// What kind of problem a finding describes (`finding.category`, decision D8).
    pub enum Category {
        /// The code does the wrong thing.
        Correctness => "correctness",
        /// A vulnerability or unsafe handling of untrusted input.
        Security => "security",
        /// Drift from the repository's own conventions or architecture.
        Convention => "convention",
        /// The change is not covered by tests that would catch its regression.
        Tests => "tests",
        /// A performance problem.
        Perf => "perf",
        /// Anything else.
        Other => "other",
    }
}

string_enum! {
    /// Where a finding is in its life (`finding.state`, SPEC §5).
    pub enum FindingState {
        /// Recorded, not yet published anywhere.
        Open => "open",
        /// Published to at least one target.
        Published => "published",
        /// The user asked never to be told this again (SPEC §5, `suppression`).
        Suppressed => "suppressed",
        /// A later run replaced it with a better statement of the same defect.
        Superseded => "superseded",
        /// Fixed.
        Resolved => "resolved",
    }
}

string_enum! {
    /// An abstract publish operation a target may support (SPEC §11.1).
    ///
    /// Targets map these onto concrete tools; the pipeline only ever names the
    /// capability, never a tool.
    pub enum Capability {
        /// A threaded review with inline comments (GitHub).
        PostReview => "post_review",
        /// A single comment on a change.
        Comment => "comment",
        /// File a work item (Andare).
        CreateIssue => "create_issue",
        /// Move a work item's state (Andare).
        SetStatus => "set_status",
        /// A pass/fail/pending check on a commit (GitHub).
        SetCheck => "set_check",
        /// Create or update a document (Trama).
        UpsertDoc => "upsert_doc",
        /// Cross-link a document and an issue (Trama ↔ Andare).
        LinkDocToIssue => "link_doc_to_issue",
    }
}

string_enum! {
    /// Lifecycle of one publish action (`publish_action.status`, SPEC §5).
    pub enum PublishActionStatus {
        /// Created, not yet dispatched.
        Pending => "pending",
        /// High risk under `auto_low_ask_high`; sitting in the approvals inbox.
        AwaitingApproval => "awaiting_approval",
        /// A human approved it; awaiting dispatch.
        Approved => "approved",
        /// A human rejected it. Terminal.
        Rejected => "rejected",
        /// Delivered; `external_ref` names what was created.
        Sent => "sent",
        /// Delivery failed after the retry policy gave up.
        Failed => "failed",
        /// The repo was in `dry_run`; recorded rather than sent (SPEC §12.2).
        SkippedDryRun => "skipped_dry_run",
    }
}

string_enum! {
    /// How dangerous one publish action is (`publish_action.risk`, SPEC §12.3).
    ///
    /// Computed per action, never per run.
    pub enum RiskClass {
        /// Additive, easily reversible, low blast radius.
        Low => "low",
        /// Blocks people, notifies broadly, or creates work.
        High => "high",
    }
}

string_enum! {
    /// The overall stance a review takes on a change (SPEC §10.2).
    pub enum Verdict {
        /// No blocking findings. Posted as a GitHub `COMMENT` review unless the
        /// repo opts into `allow_approve`; see SPEC §10.2.
        Approve => "approve",
        /// Non-blocking findings worth reading.
        Comment => "comment",
        /// At least one `critical` or `high` finding survived.
        RequestChanges => "request_changes",
    }
}

impl AutonomyMode {
    /// The effective mode for a repository, given the global ceiling.
    ///
    /// SPEC §12.2: the effective mode is `min(global_mode, repo_mode)`. The global
    /// setting is a ceiling, so a repo can never be more autonomous than the app.
    pub fn effective(global: Self, repo: Self) -> Self {
        global.min(repo)
    }

    /// Whether reviews run at all in this mode.
    pub const fn runs_reviews(self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl Severity {
    /// Whether a finding at this severity blocks a change (SPEC §10.2).
    pub const fn is_blocking(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

impl Verdict {
    /// The verdict implied by the severities that survived synthesis (SPEC §10.2).
    ///
    /// `request_changes` if any `critical`/`high` survives; `comment` if any
    /// `medium`/`low`; `approve` otherwise. `info` alone is not worth a comment
    /// verdict — it is reported in the summary body only (SPEC §10.1).
    pub fn from_severities(severities: impl IntoIterator<Item = Severity>) -> Self {
        let mut verdict = Self::Approve;
        for severity in severities {
            match severity {
                Severity::Critical | Severity::High => return Self::RequestChanges,
                Severity::Medium | Severity::Low => verdict = Self::Comment,
                Severity::Info => {}
            }
        }
        verdict
    }
}

impl RunStatus {
    /// Whether the run has finished and will not change again.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }
}
