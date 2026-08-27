use revlocal_core::{RepoId, RunId};

fn cancel(run: RunId) -> i64 {
    run.get()
}

fn main() {
    // A repo id is not a run id, even though both wrap an i64.
    let _ = cancel(RepoId::new(7));
}
