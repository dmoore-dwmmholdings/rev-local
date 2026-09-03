# rev-local

Local, autonomous code review for git and Subversion, published to GitHub,
Andare or a file on your disk.

rev-local watches repositories you already have on disk, reviews each new change
with a coding CLI you already have installed (`claude`, `codex`), and publishes the
findings back to wherever the work lives — a GitHub issue, an issue tracker,
a wiki. Everything runs on your machine: the repository is never uploaded, and the
review engine is a process you control.

## Status

**Early development.** The library and its test suite are substantial.

Working today:

- **The loop, unattended.** Turn on Autopilot and the app checks every enabled
  repository on a timer, reviews what it finds, and delivers the findings without
  anybody pressing anything. Optionally starting itself at login, because the
  daemon runs in-process and reviewing stops when the app does.
- **Three places findings go.** A local markdown report that needs no
  configuration at all, an Andare issue, and a GitHub issue — each deduplicated by
  the finding's fingerprint, so a problem that survives another commit updates
  what is already there rather than filing again.
- **The full command line.** Every command in the specification exists, and each is
  exercised by a test that reads the specification rather than a transcription of
  it. That includes `db export`, which writes the review record — repositories,
  runs and findings — as one JSON document, so last month's findings can be read
  somewhere else or kept before `db vacuum` deletes the rows.
- **Starting a review whenever you want one.** The repository screen reviews a
  branch against what it forked from, a single commit, or the whole repository —
  queued immediately, with its stages arriving as live events rather than on a
  refresh.
- git discovery, materialization and skip rules — a review never mutates the
  repository under review
- the review pipeline end to end: depth selection, diff truncation, finding
  normalization, fingerprinting and dedupe
- engine layer for `claude` and `codex`, with process supervision and an
  environment denylist that keeps credentials away from the review process
- Subversion, including branch-merge detection
- SQLite store, audit log and budget ledger
- publish queue with idempotent delivery, approval gating and per-target retry
- triggers: polling, git hooks, and a signature-checked webhook listener
- a kill switch that stops a review already running, and releases again
- housekeeping it does on its own: approvals expire, finished runs and their
  transcripts are cleared, a repository whose checkout has gone is reported
  rather than retried

**Defaults worth knowing:** `revlocal review` takes a path rather than a
configured repository, so it has no stored engine choice to honour and defaults to
the mock engine — which spends nothing and invents its findings. It says so on
stderr every time; `--engine claude` or `--engine codex` is what runs a real one.
`revlocal watch` and the desktop app use the repository's configured engine.

Not yet: reviewing a **GitHub repository at pull-request granularity**. Filing and
commenting on GitHub works — that is the "GitHub issue" above — but adding a
repository with `--kind github` is refused, because pull-request discovery has no
adapter behind it. Add such a repository as a `git` checkout and its commits are
reviewed like any other.

Every command that is not doing the whole job says so when you run it, rather than
looking like it worked.

See [USAGE.md](USAGE.md) for what you can run right now, and
[docs/OPERATIONS.md](docs/OPERATIONS.md) for what to do when something goes wrong.

## Design

Three properties the code is organised around:

- **Reviewing never writes to the repository under review.** Changes are
  materialized into a scratch worktree, and the fixture tests assert the source
  tree is byte-identical afterwards.
- **Nothing is silently dropped.** Where the system truncates, samples or skips, it
  records the fact and reports it. A review that saw 60% of a diff must never look
  like a review that saw all of it.
- **The first write to any system is high risk**, so under every mode that asks,
  a human sees it before it goes. First use is a classification rather than a
  veto: `auto` means "send without asking" and sends it, which is what choosing
  `auto` is for.

`SPEC.md` is the design specification; `docs/adr/` records the decisions that were
not obvious. Section references (§) in source comments point into `SPEC.md`.

## Building

Requires a Rust toolchain (1.82+) and `node` for the test fixtures.

```
cargo build --workspace
cargo test --workspace
```

Some tests skip cleanly when an optional tool is absent and say so rather than
passing quietly — `svn` for the Subversion fixtures, `pwsh` for the Windows
fixture-parity check.

### The desktop app

```
cargo install tauri-cli --version "^2" --locked
cd crates/revlocal-tauri
cargo tauri build --features desktop
```

This produces a `.app` and `.dmg` on macOS, `.msi` and an NSIS installer on
Windows, and `.deb` and `.AppImage` on Linux, under `target/release/bundle/`.
The front end is built by the bundler itself, so there is no separate `npm`
step to remember.

Linux additionally needs the WebKitGTK development packages:

```
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev patchelf file
```

### These builds are not signed

No code-signing identity is configured, and CI does not hold one. What that
means for you, rather than what it means in the abstract:

- **macOS** puts an unsigned app in quarantine. Double-clicking it reports that
  the app "is damaged and can't be opened", which is Gatekeeper's phrasing for
  "not notarised" and is not a claim about the file. Right-click → Open, or
  `xattr -dr com.apple.quarantine /Applications/rev-local.app`, gets past it.
- **Windows** SmartScreen shows "Windows protected your PC" for an installer
  with no reputation. More info → Run anyway.
- **Linux** does not sign desktop packages by convention, so nothing changes
  there.

Building it yourself avoids all of this, and is three commands.

If you have signing credentials, Tauri reads them from the standard environment
variables — `APPLE_SIGNING_IDENTITY` and the notarisation pair on macOS, a
`certificateThumbprint` in `tauri.conf.json` on Windows. They are deliberately
absent rather than blank: an empty identity is a value that something downstream
has to decide is not a certificate, and Tauri reads it as one.

## License

Apache-2.0. See [LICENSE](LICENSE).
