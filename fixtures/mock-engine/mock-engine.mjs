#!/usr/bin/env node
// A stand-in for a real review engine (SPEC §8.2, §16.2).
//
// The point of this fixture is that the ENGINE LAYER'S FALLBACK LADDER can be
// tested without spending a token or touching a network. Every rung of §8.2 has a
// mode here, selected by MOCK_ENGINE_MODE, so a test can ask for the failure it
// wants instead of hoping a real CLI produces it.
//
// It honours REVLOCAL_OUT the way §8.2 requires: the runner creates the directory
// and passes it in the environment, the engine writes result.json there, and
// writes nothing else to it.
//
// Node rather than shell because acceptance criterion 4 requires this to run on
// Windows. `run` and `run.cmd` are thin shims onto this file.

import { writeFileSync } from "node:fs";
import { join } from "node:path";

const MODES = [
  "valid",
  "malformed_json",
  "fenced_only",
  "no_file",
  "hang",
  "partial_findings",
  "nonzero_exit",
  "slow_but_ok",
];

const VERSION = "mock-engine 1.0.0";

// A result that validates against crates/revlocal-engine/schema/result.v1.json.
// The findings mirror the bugs planted in the git fixture, so a pipeline test can
// assert on a specific claim rather than merely on a count.
function validResult() {
  return {
    schema_version: 1,
    verdict: "request_changes",
    summary:
      "Two defects: an off-by-one in the pager and an unparameterised SQL query.",
    findings: [
      {
        severity: "high",
        category: "correctness",
        confidence: 0.9,
        file: "src/pager.rs",
        line_start: 6,
        line_end: 6,
        title: "Inclusive range walks one past the last index",
        body: "`start..=(start + per_page)` yields `per_page + 1` indices.",
        failure_scenario:
          "items.len() == 10, per_page == 10, page == 0 -> indexes items[10] and panics.",
        suggested_fix: "Use `start..(start + per_page)`.",
      },
      {
        severity: "critical",
        category: "security",
        confidence: 0.95,
        file: "src/db.rs",
        line_start: 4,
        line_end: 5,
        title: "User input is interpolated into SQL",
        body: "`name` is formatted into the query, so it is executed as SQL.",
        failure_scenario:
          "name = \"' OR '1'='1\" returns every row in users.",
        suggested_fix: "Bind `name` as a parameter.",
      },
    ],
  };
}

function writeResult(outDir, contents) {
  writeFileSync(join(outDir, "result.json"), contents, "utf8");
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function main() {
  const argv = process.argv.slice(2);

  // `revlocal doctor` probes each engine with version_args first (SPEC §8.4).
  if (argv.includes("--version") || argv.includes("-V")) {
    process.stdout.write(`${VERSION}\n`);
    return 0;
  }

  const mode = process.env.MOCK_ENGINE_MODE ?? "valid";
  if (!MODES.includes(mode)) {
    process.stderr.write(
      `mock-engine: unknown MOCK_ENGINE_MODE ${JSON.stringify(mode)}; ` +
        `expected one of: ${MODES.join(", ")}\n`,
    );
    return 64; // EX_USAGE
  }

  const outDir = process.env.REVLOCAL_OUT;
  if (!outDir && mode !== "hang") {
    // Failing loudly beats writing nowhere: a test whose REVLOCAL_OUT was not
    // plumbed through would otherwise look like the `no_file` rung.
    process.stderr.write("mock-engine: REVLOCAL_OUT is not set\n");
    return 64;
  }

  switch (mode) {
    // Rung 0 — the happy path. result.json is present and valid.
    case "valid":
      writeResult(outDir, JSON.stringify(validResult(), null, 2));
      process.stdout.write("mock-engine: wrote result.json\n");
      return 0;

    // Rung (a) — result.json exists but is not parseable, so the runner falls
    // through to the last fenced ```json block in stdout.
    case "malformed_json":
      writeResult(outDir, '{"schema_version": 1, "verdict": "approve",');
      process.stdout.write("Here is my review.\n\n");
      process.stdout.write("```json\n");
      process.stdout.write(JSON.stringify(validResult(), null, 2));
      process.stdout.write("\n```\n");
      return 0;

    // Rung (a) again, without a file at all: the fenced block is the only source.
    // Two fenced blocks, because §8.2 says the LAST one is authoritative and a
    // runner that takes the first would pass a single-block test.
    case "fenced_only":
      process.stdout.write("```json\n");
      process.stdout.write(
        JSON.stringify({ schema_version: 1, verdict: "approve", summary: "draft", findings: [] }),
      );
      process.stdout.write("\n```\n\nOn reflection:\n\n");
      process.stdout.write("```json\n");
      process.stdout.write(JSON.stringify(validResult(), null, 2));
      process.stdout.write("\n```\n");
      return 0;

    // Rung (b) — no file, no fence; stdout is bare JSON.
    case "no_file":
      process.stdout.write(JSON.stringify(validResult()));
      process.stdout.write("\n");
      return 0;

    // §8.5 — SIGTERM, 5s grace, SIGKILL. The handler is installed and does
    // nothing, so SIGTERM is genuinely ignored and the SIGKILL path is exercised
    // rather than assumed.
    case "hang": {
      process.on("SIGTERM", () => {});
      process.on("SIGINT", () => {});

      // A grandchild, so a supervisor that kills only the direct child is caught.
      // That is the failure worth catching: a surviving grandchild holds the
      // scratch worktree open and the next run fails for a reason nobody can trace.
      // Its pid goes to a file rather than stdout, because a supervisor under test
      // may never read stdout from a process it had to kill.
      const { spawn } = await import("node:child_process");
      const grandchild = spawn(process.execPath, ["-e", "setInterval(() => {}, 1 << 30)"], {
        detached: false,
        stdio: "ignore",
      });
      if (outDir) {
        writeFileSync(join(outDir, "grandchild.pid"), String(grandchild.pid), "utf8");
      }

      process.stdout.write("mock-engine: hanging, ignoring SIGTERM\n");
      // An interval keeps the event loop alive indefinitely without spinning.
      setInterval(() => {}, 1 << 30);
      await new Promise(() => {});
      return 0;
    }

    // §8.3 — findings failing validation are dropped individually with an audit
    // event; the run still succeeds if the envelope parsed. One finding here is
    // missing `body` and one has a title over the 80-character cap.
    case "partial_findings": {
      const result = validResult();
      result.findings.push({
        severity: "low",
        category: "convention",
        title: "This finding has no body",
      });
      result.findings.push({
        severity: "info",
        category: "other",
        title: "x".repeat(120),
        body: "The title exceeds the 80-character cap in SPEC §5 and §8.3.",
      });
      writeResult(outDir, JSON.stringify(result, null, 2));
      return 0;
    }

    // The engine failed but still produced usable output. A runner that keys only
    // off the exit code would throw the review away.
    case "nonzero_exit":
      writeResult(outDir, JSON.stringify(validResult(), null, 2));
      process.stderr.write("mock-engine: simulated failure after writing output\n");
      return 3;

    // Slower than instant, but within any real timeout. Distinguishes "slow" from
    // "hung" so a timeout test cannot pass by being merely impatient.
    case "slow_but_ok": {
      const delayMs = Number(process.env.MOCK_ENGINE_DELAY_MS ?? "750");
      await sleep(delayMs);
      writeResult(outDir, JSON.stringify(validResult(), null, 2));
      process.stdout.write(`mock-engine: wrote result.json after ${delayMs}ms\n`);
      return 0;
    }

    default:
      return 70; // unreachable: MODES is checked above
  }
}

main().then(
  (code) => process.exit(code),
  (err) => {
    process.stderr.write(`mock-engine: ${err?.stack ?? err}\n`);
    process.exit(70);
  },
);
