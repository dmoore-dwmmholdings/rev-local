use revlocal_core::RunId;

fn cancel(run: RunId) -> i64 {
    run.get()
}

fn main() {
    // `.into()` must not be able to infer a RunId from an integer either.
    let raw: i64 = 7;
    let _ = cancel(raw.into());
}
