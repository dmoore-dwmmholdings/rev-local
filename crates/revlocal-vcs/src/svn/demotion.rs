//! Pseudo-PR authority and demotion of constituent reviews (RL-905, SPEC §6.4).
//!
//! # The problem a pseudo-PR creates
//!
//! RL-904 makes a reintegration produce two changes: the merge revision, and a
//! synthetic one whose diff is the whole branch. That is the right thing to
//! review — but the branch's revisions were *already* reviewed on their way in, if
//! `watch_branches` is on. So the same defect can be found twice: once on the
//! revision that introduced it, once on the pseudo-PR that contains it.
//!
//! Filing it twice is worse than filing it once and worse than filing it zero
//! times, because a reviewer who resolves one copy still sees the other and stops
//! trusting the tool.
//!
//! §6.4 resolves it by rank rather than by suppression: **the pseudo-PR review is
//! authoritative**, and the per-revision findings it duplicates are demoted to
//! `info`. Demoted, not deleted — §18. The row still exists, still says what it
//! said, and now says which review superseded it.
//!
//! # Why demote the per-revision copy and not the pseudo-PR's
//!
//! The two findings are the same defect seen through different windows. The
//! per-revision one saw the commit that introduced it; the pseudo-PR one saw it in
//! the context of the whole branch, with the rest of the branch's code around it —
//! including any later revision that already fixed it. The wider window is the
//! better one to keep, and it is the one a human would look at if asked "is this
//! branch ready to merge?".
//!
//! # Matching is by fingerprint, which is the only thing that survives
//!
//! §10.3's fingerprint is deliberately line-number independent, because a finding
//! has to survive a rebase. That is exactly the property needed here: the same
//! defect sits at a different line in the branch diff than in the revision diff,
//! so anything that matched on position would match nothing.

use std::collections::BTreeMap;

use revlocal_core::{Finding, Severity};

use super::cmd::{SvnError, SvnRunner};

/// The revisions a pseudo-PR subsumes: the branch's own work, fork exclusive.
///
/// `--stop-on-copy` again, for the reason RL-904 needed it — an unrestricted log
/// on a branch walks back through the copy into trunk's history, and every trunk
/// revision it returned would be wrongly treated as the branch's work and have its
/// findings demoted.
pub async fn constituent_revisions(
    runner: &SvnRunner,
    repo_url: &str,
    branch: &str,
    through: u64,
) -> Result<Vec<u64>, SvnError> {
    let branch_path = if branch.starts_with('/') {
        branch.to_owned()
    } else {
        format!("/{branch}")
    };
    let target = format!("{}{branch_path}", repo_url.trim_end_matches('/'));
    let range = format!("1:{through}");

    let output = runner
        .run(
            std::path::Path::new("."),
            &["log", "--xml", "--stop-on-copy", "-r", &range, &target],
        )
        .await?;

    let mut revisions: Vec<u64> = super::discover::parse_log_xml(&output.stdout)?
        .into_iter()
        .map(|revision| revision.revision)
        .collect();
    revisions.sort_unstable();

    // The oldest entry is the copy that created the branch. It is trunk's content
    // at the fork, not the branch's work, so it is not a constituent.
    if !revisions.is_empty() {
        revisions.remove(0);
    }
    Ok(revisions)
}

/// What happens to a per-revision finding once the pseudo-PR is authoritative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Filed at its own severity. The pseudo-PR did not find this one.
    Filed,
    /// Kept, recorded, and dropped to `info` because the pseudo-PR filed it.
    ///
    /// Carries both halves of the answer to "why is this only an info?", because
    /// a demotion whose reason is not written down is indistinguishable from a
    /// severity the engine simply got wrong.
    Demoted {
        /// The pseudo-PR change that filed the authoritative copy.
        superseded_by: String,
        /// A sentence for the publish plan and the UI.
        reason: String,
    },
}

impl Disposition {
    /// Whether this finding was demoted.
    pub const fn is_demoted(&self) -> bool {
        matches!(self, Self::Demoted { .. })
    }

    /// The severity this finding publishes at.
    ///
    /// §6.4 names `info` exactly. `info` is "an observation with no action
    /// implied", which is what a duplicate of an already-filed defect is.
    pub const fn effective_severity(&self, original: Severity) -> Severity {
        match self {
            Self::Filed => original,
            Self::Demoted { .. } => Severity::Info,
        }
    }
}

/// One per-revision finding and what the plan does with it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedFinding {
    /// §10.3's fingerprint.
    pub fingerprint: String,
    /// The revision whose review produced it.
    pub revision: Option<u64>,
    /// What the engine said it was.
    pub original_severity: Severity,
    /// What it will be published as.
    pub effective_severity: Severity,
    /// The claim, for the plan's human-readable form.
    pub title: String,
    /// Filed, or demoted and why.
    pub disposition: Disposition,
}

/// What publishing will do with a reintegration's findings (§6.4).
///
/// Every finding that went in comes out, which is the property that makes this a
/// plan rather than a filter. A caller cannot accidentally publish a subset by
/// forgetting a branch of a match.
#[derive(Debug, Clone, PartialEq)]
pub struct DemotionPlan {
    /// The pseudo-PR change that is authoritative for this merge.
    pub authoritative: String,
    /// The pseudo-PR's own findings, always filed.
    pub authoritative_findings: Vec<PlannedFinding>,
    /// The per-revision findings, filed or demoted.
    pub constituent_findings: Vec<PlannedFinding>,
}

impl DemotionPlan {
    /// How many per-revision findings were demoted.
    pub fn demoted_count(&self) -> usize {
        self.constituent_findings
            .iter()
            .filter(|planned| planned.disposition.is_demoted())
            .count()
    }

    /// Every finding the plan will publish, at the severity it will publish at.
    pub fn all(&self) -> impl Iterator<Item = &PlannedFinding> {
        self.authoritative_findings
            .iter()
            .chain(self.constituent_findings.iter())
    }

    /// The lines the publish plan shows a human (§18: a demotion is visible).
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "{} is authoritative for this merge; {} of {} per-revision finding(s) demoted to info",
            self.authoritative,
            self.demoted_count(),
            self.constituent_findings.len()
        )];

        for planned in &self.constituent_findings {
            if let Disposition::Demoted { reason, .. } = &planned.disposition {
                lines.push(format!(
                    "  {} ({} -> info): {reason}",
                    planned.title, planned.original_severity
                ));
            }
        }
        lines
    }
}

/// Build the plan (§6.4).
///
/// `constituents` maps a revision number to the findings its review produced.
pub fn plan(
    pseudo_pr_external_id: &str,
    pseudo_pr_findings: &[Finding],
    constituents: &BTreeMap<u64, Vec<Finding>>,
) -> DemotionPlan {
    let authoritative: std::collections::BTreeSet<&str> = pseudo_pr_findings
        .iter()
        .map(|finding| finding.fingerprint.as_str())
        .collect();

    let authoritative_findings = pseudo_pr_findings
        .iter()
        .map(|finding| PlannedFinding {
            fingerprint: finding.fingerprint.clone(),
            revision: None,
            original_severity: finding.severity,
            effective_severity: finding.severity,
            title: finding.title.clone(),
            disposition: Disposition::Filed,
        })
        .collect();

    let mut constituent_findings = Vec::new();
    for (revision, findings) in constituents {
        for finding in findings {
            let disposition = if authoritative.contains(finding.fingerprint.as_str()) {
                Disposition::Demoted {
                    superseded_by: pseudo_pr_external_id.to_owned(),
                    reason: format!(
                        "the same defect was filed on {pseudo_pr_external_id}, which is \
                         authoritative for this merge (SPEC §6.4)"
                    ),
                }
            } else {
                // Found on the branch and not in the branch-vs-trunk diff. Usually
                // this means a later revision fixed it — which is worth keeping at
                // its own severity rather than demoting, because "we found this and
                // you fixed it" is a different statement from "we found this twice".
                Disposition::Filed
            };

            constituent_findings.push(PlannedFinding {
                fingerprint: finding.fingerprint.clone(),
                revision: Some(*revision),
                original_severity: finding.severity,
                effective_severity: disposition.effective_severity(finding.severity),
                title: finding.title.clone(),
                disposition,
            });
        }
    }

    DemotionPlan {
        authoritative: pseudo_pr_external_id.to_owned(),
        authoritative_findings,
        constituent_findings,
    }
}

/// The constituent findings to hand the pseudo-PR's prompt as prior context.
///
/// §6.4's first half, and the reason the demotion in its second half is mostly a
/// no-op in practice: an engine told "these were already found on the branch" will
/// usually report the same fingerprints, which is what makes them matchable. The
/// demotion is the safety net for when it does not.
///
/// Deduplicated by fingerprint — the same defect found on three consecutive branch
/// revisions is one piece of prior context, not three. Ordered by fingerprint so
/// the prompt is byte-identical across runs (ADR 0024).
pub fn prior_context(constituents: &BTreeMap<u64, Vec<Finding>>) -> Vec<Finding> {
    let mut by_fingerprint: BTreeMap<&str, &Finding> = BTreeMap::new();

    for findings in constituents.values() {
        for finding in findings {
            by_fingerprint
                .entry(finding.fingerprint.as_str())
                // Keep the worst sighting: a defect reported `high` once and `low`
                // twice is a `high` the engine should be told about.
                .and_modify(|kept| {
                    if finding.severity > kept.severity {
                        *kept = finding;
                    }
                })
                .or_insert(finding);
        }
    }

    by_fingerprint.into_values().cloned().collect()
}
