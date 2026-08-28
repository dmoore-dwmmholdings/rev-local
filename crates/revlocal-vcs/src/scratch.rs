//! The scratch directory lifecycle (SPEC §6.1, §13.1).
//!
//! Materializing a change never touches the user's checkout. It happens in a
//! scratch directory under `{data_dir}/scratch/{run_id}/`, which is removed when
//! the run terminates — unless the run failed and `keep_scratch_on_failure` is set,
//! in which case it is left for someone to look at.
//!
//! # The default is "failed"
//!
//! [`ScratchDir`] starts in [`RunOutcome::Failed`] and only becomes successful when
//! a caller says so with [`mark_succeeded`](ScratchDir::mark_succeeded). That is
//! deliberate and it is the whole reason this is RAII rather than a cleanup call at
//! the end of the happy path.
//!
//! A scratch directory is dropped on every path out of a run, including the ones
//! nobody wrote: an early `?`, a cancellation, a panic unwinding through the
//! pipeline. Every one of those *is* a failure, and it is exactly when the
//! materialized tree is worth keeping. Defaulting to success would delete the
//! evidence on precisely the runs someone needs to debug — and it would do it
//! silently.

use std::path::{Path, PathBuf};

use revlocal_core::RunId;

/// How a run ended, as far as its scratch directory is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The run reached a successful terminal state.
    Succeeded,
    /// Anything else, including an unwind. The default.
    Failed,
}

/// A scratch directory that removes itself when dropped.
///
/// Created under `{data_dir}/scratch/{run_id}/`, so two runs — including two runs
/// on the same repository — cannot collide. The `run_id` is the isolation
/// mechanism; nothing about the repository appears in the path.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
    outcome: RunOutcome,
    keep_on_failure: bool,
    /// Set when the caller has taken the directory over and drop must not remove it.
    disarmed: bool,
}

impl ScratchDir {
    /// Create the scratch directory for `run_id` under `data_dir`.
    ///
    /// Fails if the directory already exists, rather than reusing it: a collision
    /// means two runs share a `run_id`, and silently sharing a worktree between
    /// them would produce a review of the wrong tree.
    pub fn create(data_dir: &Path, run_id: RunId, keep_on_failure: bool) -> std::io::Result<Self> {
        let path = Self::path_for(data_dir, run_id);

        if path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "scratch directory {} already exists; two runs share run_id {run_id}",
                    path.display()
                ),
            ));
        }

        std::fs::create_dir_all(&path)?;

        Ok(Self {
            path,
            outcome: RunOutcome::Failed,
            keep_on_failure,
            disarmed: false,
        })
    }

    /// Where a run's scratch directory lives.
    pub fn path_for(data_dir: &Path, run_id: RunId) -> PathBuf {
        data_dir.join("scratch").join(run_id.get().to_string())
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this scratch will survive being dropped.
    pub const fn will_be_kept(&self) -> bool {
        self.disarmed || (self.keep_on_failure && matches!(self.outcome, RunOutcome::Failed))
    }

    /// Record that the run succeeded, so the directory is removed on drop.
    ///
    /// Takes `&mut self` rather than consuming, so it can be called at the end of a
    /// pipeline stage without restructuring ownership.
    pub fn mark_succeeded(&mut self) {
        self.outcome = RunOutcome::Succeeded;
    }

    /// Record that the run failed. Only useful to undo a premature success.
    pub fn mark_failed(&mut self) {
        self.outcome = RunOutcome::Failed;
    }

    /// The outcome recorded so far.
    pub const fn outcome(&self) -> RunOutcome {
        self.outcome
    }

    /// Give up ownership of the directory: drop will not remove it.
    ///
    /// Returns the path. For `revlocal scratch keep`-style escape hatches and for
    /// tests that need to inspect the tree after the guard is gone.
    pub fn into_kept_path(mut self) -> PathBuf {
        self.disarmed = true;
        self.path.clone()
    }

    /// Remove the directory now, reporting failure instead of swallowing it.
    ///
    /// `Drop` cannot report anything, so a caller that wants to know whether
    /// cleanup worked has to ask for it explicitly.
    pub fn remove_now(mut self) -> std::io::Result<()> {
        self.disarmed = true;
        Self::remove(&self.path)
    }

    fn remove(path: &Path) -> std::io::Result<()> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            // Already gone is the outcome we wanted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if self.will_be_kept() {
            if !self.disarmed {
                // Kept on purpose, so say so: a directory left behind with no
                // explanation looks like a leak, and someone will delete it.
                tracing::info!(
                    scratch = %self.path.display(),
                    "keeping the scratch directory: the run failed and \
                     keep_scratch_on_failure is set"
                );
            }
            return;
        }

        if let Err(error) = Self::remove(&self.path) {
            // A failed cleanup must not panic during an unwind — that would abort
            // the process and lose the original error, which is the one that
            // mattered.
            tracing::warn!(
                scratch = %self.path.display(),
                %error,
                "could not remove the scratch directory"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A data directory to hang scratch dirs off.
    fn data_dir() -> TempDir {
        TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"))
    }

    fn create(root: &Path, run: i64, keep_on_failure: bool) -> ScratchDir {
        ScratchDir::create(root, RunId::new(run), keep_on_failure)
            .unwrap_or_else(|e| panic!("creating scratch: {e}"))
    }

    #[test]
    fn scratch_is_removed_on_drop_for_a_successful_run() {
        let data = data_dir();
        let path;
        {
            let mut scratch = create(data.path(), 1, true);
            path = scratch.path().to_path_buf();
            assert!(
                path.is_dir(),
                "the directory must exist while the guard does"
            );

            std::fs::write(path.join("materialized.txt"), "tree")
                .unwrap_or_else(|e| panic!("write: {e}"));

            scratch.mark_succeeded();
        }
        assert!(
            !path.exists(),
            "a successful run must not leave its scratch behind"
        );
    }

    #[test]
    fn scratch_survives_a_failed_run_when_keep_on_failure_is_set() {
        let data = data_dir();
        let path;
        {
            let scratch = create(data.path(), 2, true);
            path = scratch.path().to_path_buf();
            std::fs::write(path.join("engine.log"), "what went wrong")
                .unwrap_or_else(|e| panic!("write: {e}"));
            // No mark_succeeded: the run failed.
        }
        assert!(
            path.is_dir(),
            "a failed run's scratch must survive for debugging"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("engine.log")).unwrap_or_default(),
            "what went wrong",
            "and its contents must be intact"
        );
    }

    #[test]
    fn scratch_is_removed_after_a_failed_run_when_keep_on_failure_is_not_set() {
        let data = data_dir();
        let path;
        {
            let scratch = create(data.path(), 3, false);
            path = scratch.path().to_path_buf();
        }
        assert!(
            !path.exists(),
            "without keep_scratch_on_failure, nothing is kept"
        );
    }

    #[test]
    fn scratch_defaults_to_failed_so_an_early_exit_keeps_the_evidence() {
        // The reason this is RAII rather than a cleanup call at the end of the
        // happy path. A scratch is dropped on every route out of a run, including
        // an early `?` nobody wrote a branch for — and every one of those is a
        // failure.
        let data = data_dir();
        let scratch = create(data.path(), 4, true);
        assert_eq!(scratch.outcome(), RunOutcome::Failed);
        assert!(scratch.will_be_kept());
    }

    #[test]
    fn scratch_survives_a_panic_unwinding_through_the_run() {
        // The case that motivates the default. A panic is the least likely path to
        // have been thought about and the most likely to need the tree afterwards.
        let data = data_dir();
        let root = data.path().to_path_buf();
        let expected = ScratchDir::path_for(&root, RunId::new(5));

        let result = std::panic::catch_unwind(move || {
            let _scratch = create(&root, 5, true);
            panic!("the pipeline exploded");
        });

        assert!(result.is_err(), "the panic must have propagated");
        assert!(
            expected.is_dir(),
            "a panic is a failed run; its scratch must survive when keep is set"
        );
    }

    #[test]
    fn scratch_a_panic_still_cleans_up_when_keep_is_not_set() {
        let data = data_dir();
        let root = data.path().to_path_buf();
        let expected = ScratchDir::path_for(&root, RunId::new(6));

        let result = std::panic::catch_unwind(move || {
            let _scratch = create(&root, 6, false);
            panic!("the pipeline exploded");
        });

        assert!(result.is_err());
        assert!(!expected.exists(), "cleanup must still happen on an unwind");
    }

    #[test]
    fn scratch_two_concurrent_runs_on_one_repo_are_isolated() {
        // Nothing about the repository appears in the path — the run id is the
        // isolation mechanism, which is what lets the same repo be reviewed twice
        // at once (SPEC §4.3 allows two concurrent runs by default).
        let data = data_dir();
        let mut first = create(data.path(), 10, false);
        let mut second = create(data.path(), 11, false);

        assert_ne!(first.path(), second.path());

        std::fs::write(first.path().join("marker"), "first")
            .unwrap_or_else(|e| panic!("write: {e}"));
        std::fs::write(second.path().join("marker"), "second")
            .unwrap_or_else(|e| panic!("write: {e}"));

        assert_eq!(
            std::fs::read_to_string(first.path().join("marker")).unwrap_or_default(),
            "first",
            "one run's tree must not be visible in the other's"
        );

        // ...and finishing one must not remove the other's directory.
        let second_path = second.path().to_path_buf();
        first.mark_succeeded();
        drop(first);
        assert!(
            second_path.is_dir(),
            "finishing one run must not disturb the other"
        );

        second.mark_succeeded();
    }

    #[test]
    fn scratch_refuses_to_reuse_a_directory_that_already_exists() {
        // A collision means two runs share a run_id. Silently reusing the directory
        // would review one run's tree under the other's id, and the result would
        // look like a correct review of the wrong thing.
        let data = data_dir();
        let _first = create(data.path(), 20, true);

        let second = ScratchDir::create(data.path(), RunId::new(20), true);
        let error = second
            .err()
            .unwrap_or_else(|| panic!("a collision must be refused"));
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(
            error.to_string().contains("20"),
            "the error must name the run: {error}"
        );
    }

    #[test]
    fn scratch_can_be_handed_over_and_then_is_not_removed() {
        let data = data_dir();
        let path = {
            let scratch = create(data.path(), 30, false);
            scratch.into_kept_path()
        };
        assert!(
            path.is_dir(),
            "a directory handed over must not be removed on drop"
        );
        // Not leaked: the enclosing TempDir still owns it.
    }

    #[test]
    fn scratch_remove_now_reports_failure_rather_than_swallowing_it() {
        // Drop cannot report anything, so a caller that needs to know asks.
        let data = data_dir();
        let scratch = create(data.path(), 40, true);
        let path = scratch.path().to_path_buf();

        scratch
            .remove_now()
            .unwrap_or_else(|e| panic!("removal should succeed: {e}"));
        assert!(!path.exists());
    }

    #[test]
    fn scratch_removing_an_already_gone_directory_is_not_an_error() {
        // The outcome the caller wanted is the outcome they got.
        let data = data_dir();
        let scratch = create(data.path(), 50, false);
        let path = scratch.path().to_path_buf();
        std::fs::remove_dir_all(&path).unwrap_or_else(|e| panic!("setup: {e}"));

        assert!(scratch.remove_now().is_ok());
    }

    #[test]
    fn scratch_paths_live_under_data_dir_scratch_run_id() {
        // SPEC §6.1 names this layout, and the CLI's `scratch` subcommands and the
        // startup pruner will both have to find it.
        let path = ScratchDir::path_for(Path::new("/data"), RunId::new(7));
        assert_eq!(path, Path::new("/data/scratch/7"));
    }

    #[test]
    fn scratch_marking_succeeded_can_be_undone() {
        let data = data_dir();
        let mut scratch = create(data.path(), 60, true);

        scratch.mark_succeeded();
        assert!(!scratch.will_be_kept());

        scratch.mark_failed();
        assert!(
            scratch.will_be_kept(),
            "a late failure must restore the keep behaviour"
        );
    }
}
