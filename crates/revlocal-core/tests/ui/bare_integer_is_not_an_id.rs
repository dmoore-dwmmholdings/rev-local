use revlocal_core::RunId;

fn cancel(run: RunId) -> i64 {
    run.get()
}

fn main() {
    // There is no From<i64>, so a bare row id cannot drift across the boundary.
    let _ = cancel(7);
}
