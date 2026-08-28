#!/usr/bin/env node
// Self-test for the mock MCP server. This is RL-204's gate.
//
// It drives the server the way rev-local's MCP client will — spawn, initialize,
// tools/list, tools/call over stdio JSON-RPC — and asserts the behaviours the
// downstream tests depend on. If this passes, a test that later fails is telling
// you about the client, not the fixture.

import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, "server.js");

let failures = 0;
let checks = 0;

function check(condition, description, detail) {
  checks += 1;
  if (condition) {
    process.stdout.write(`  ok   ${description}\n`);
  } else {
    failures += 1;
    process.stdout.write(`  FAIL ${description}\n`);
    if (detail !== undefined) {
      process.stdout.write(`       ${JSON.stringify(detail)}\n`);
    }
  }
}

/** A live server process with request/response plumbing. */
function startServer(env = {}) {
  const child = spawn(process.execPath, [SERVER], {
    env: { ...process.env, ...env },
    stdio: ["pipe", "pipe", "pipe"],
  });

  const pending = new Map();
  let buffer = "";

  child.stdout.on("data", (chunk) => {
    buffer += chunk.toString("utf8");
    let newline;
    while ((newline = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      if (line === "") continue;
      const message = JSON.parse(line);
      const resolve = pending.get(message.id);
      if (resolve) {
        pending.delete(message.id);
        resolve(message);
      }
    }
  });

  let nextId = 1;

  return {
    request(method, params) {
      const id = nextId++;
      const promise = new Promise((resolve, reject) => {
        pending.set(id, resolve);
        setTimeout(() => reject(new Error(`timed out waiting for ${method}`)), 5000);
      });
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
      return promise;
    },
    notify(method, params) {
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
    },
    stop() {
      child.stdin.end();
      child.kill();
    },
  };
}

function readJournal(path) {
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8")
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line) => JSON.parse(line));
}

// --- the checks -------------------------------------------------------------

async function checkHandshakeAndDiscovery() {
  process.stdout.write("handshake and discovery\n");
  const dir = mkdtempSync(join(tmpdir(), "mock-mcp-"));
  const journalPath = join(dir, "journal.jsonl");
  const server = startServer({ MOCK_MCP_JOURNAL: journalPath });

  try {
    const init = await server.request("initialize", { protocolVersion: "2025-06-18" });
    check(init.result?.serverInfo?.name?.startsWith("mock-mcp/"), "initialize returns serverInfo");
    check(Boolean(init.result?.capabilities?.tools), "initialize advertises tool capability");

    server.notify("notifications/initialized");

    const list = await server.request("tools/list");
    const names = (list.result?.tools ?? []).map((t) => t.name);
    check(names.includes("create_issue"), "the default profile exposes create_issue", names);
    check(
      (list.result?.tools ?? []).every((t) => t.inputSchema?.type === "object"),
      "every tool reports an input schema",
    );

    const journal = readJournal(journalPath);
    const kinds = journal.map((e) => e.kind);
    check(kinds[0] === "initialize", "the journal records initialize first", kinds);
    check(kinds.includes("tools/list"), "the journal records tools/list", kinds);
    check(
      journal.every((e, index) => e.seq === index + 1),
      "journal sequence numbers are dense and ordered",
      journal.map((e) => e.seq),
    );
  } finally {
    server.stop();
  }
}

async function checkToolNamesAreAProfile() {
  process.stdout.write("tool names are configurable\n");
  const server = startServer({
    MOCK_MCP_PROFILE: join(HERE, "profiles", "andare-renamed.json"),
  });

  try {
    await server.request("initialize", {});
    const list = await server.request("tools/list");
    const names = (list.result?.tools ?? []).map((t) => t.name);

    // The point of the fixture: rev-local looks for `create_issue` first and must
    // resolve to whatever the server actually calls it (SPEC §11.2).
    check(names.includes("create_work_item"), "the renamed profile exposes create_work_item", names);
    check(!names.includes("create_issue"), "...and NOT create_issue", names);

    const called = await server.request("tools/call", {
      name: "create_work_item",
      arguments: { summary: "a finding", description: "body", projectKey: "REVL" },
    });
    check(called.result?.isError === false, "the renamed tool can be called", called);
  } finally {
    server.stop();
  }
}

async function checkJournalRecordsArgsAndOrder() {
  process.stdout.write("journal records method, args and ordering\n");
  const dir = mkdtempSync(join(tmpdir(), "mock-mcp-"));
  const journalPath = join(dir, "journal.jsonl");
  const server = startServer({ MOCK_MCP_JOURNAL: journalPath });

  try {
    await server.request("initialize", {});
    await server.request("tools/call", {
      name: "create_issue",
      arguments: { title: "first", body: "b" },
    });
    await server.request("tools/call", {
      name: "create_issue",
      arguments: { title: "second", body: "b" },
    });

    const calls = readJournal(journalPath).filter((e) => e.kind === "tools/call");
    check(calls.length === 2, "both calls were journalled", calls.length);
    check(calls[0].args?.title === "first", "arguments are recorded", calls[0]);
    check(
      calls[0].seq < calls[1].seq,
      "ordering is recoverable from the journal",
      calls.map((c) => c.seq),
    );
  } finally {
    server.stop();
  }
}

async function checkReadBeforeWrite() {
  process.stdout.write("update_page requires a preceding get_page (SPEC §11.5)\n");
  const dir = mkdtempSync(join(tmpdir(), "mock-mcp-"));
  const journalPath = join(dir, "journal.jsonl");
  const server = startServer({ MOCK_MCP_JOURNAL: journalPath });

  try {
    await server.request("initialize", {});

    const blind = await server.request("tools/call", {
      name: "update_page",
      arguments: { space: "ENG", title: "Review", markdown: "new body" },
    });
    check(blind.result?.isError === true, "a blind update_page is refused", blind.result);
    check(
      String(blind.result?.content?.[0]?.text ?? "").includes("get_page"),
      "...and the refusal says what was missing",
      blind.result?.content?.[0]?.text,
    );

    // Reading a DIFFERENT page must not license the write. A rule that tracked
    // "has read anything" would be satisfied by any startup read and would let
    // every page be overwritten blind.
    await server.request("tools/call", {
      name: "get_page",
      arguments: { space: "ENG", title: "Something Else" },
    });
    const wrongPage = await server.request("tools/call", {
      name: "update_page",
      arguments: { space: "ENG", title: "Review", markdown: "new body" },
    });
    check(
      wrongPage.result?.isError === true,
      "reading one page does not license writing another",
      wrongPage.result,
    );

    await server.request("tools/call", {
      name: "get_page",
      arguments: { space: "ENG", title: "Review" },
    });
    const allowed = await server.request("tools/call", {
      name: "update_page",
      arguments: { space: "ENG", title: "Review", markdown: "new body" },
    });
    check(allowed.result?.isError === false, "update_page after get_page succeeds", allowed.result);

    const violations = readJournal(journalPath).filter(
      (e) => e.kind === "read_before_write_violation",
    );
    check(violations.length === 2, "violations are journalled", violations.length);
  } finally {
    server.stop();
  }
}

async function checkInducedFailures() {
  process.stdout.write("failures on demand, for retry tests\n");

  for (const [mode, code, retryable] of [
    ["rate_limit", -32001, true],
    ["server_error", -32002, true],
    ["invalid_params", -32602, false],
  ]) {
    const server = startServer({ MOCK_MCP_FAIL_MODE: mode });
    try {
      await server.request("initialize", {});
      const response = await server.request("tools/call", {
        name: "create_issue",
        arguments: { title: "t", body: "b" },
      });
      check(response.error?.code === code, `${mode} returns code ${code}`, response.error);
      check(
        response.error?.data?.retryable === retryable,
        `${mode} is marked retryable=${retryable}`,
        response.error?.data,
      );
    } finally {
      server.stop();
    }
  }

  // Fail N times then succeed: what a retry policy actually has to get right.
  const server = startServer({ MOCK_MCP_FAIL_MODE: "rate_limit", MOCK_MCP_FAIL_TIMES: "2" });
  try {
    await server.request("initialize", {});
    const args = { name: "create_issue", arguments: { title: "t", body: "b" } };
    const first = await server.request("tools/call", args);
    const second = await server.request("tools/call", args);
    const third = await server.request("tools/call", args);

    check(first.error !== undefined, "attempt 1 fails");
    check(second.error !== undefined, "attempt 2 fails");
    check(third.error === undefined, "attempt 3 succeeds", third);
    check(
      first.error?.data?.retry_after_ms > 0,
      "a rate limit says how long to wait rather than making the client guess",
      first.error?.data,
    );
  } finally {
    server.stop();
  }
}

async function checkUnmappableProfile() {
  process.stdout.write("a server with nothing bindable\n");
  const server = startServer({ MOCK_MCP_PROFILE: join(HERE, "profiles", "unmappable.json") });
  try {
    await server.request("initialize", {});
    const list = await server.request("tools/list");
    const names = (list.result?.tools ?? []).map((t) => t.name);
    check(names.length === 1 && names[0] === "ping", "exposes only an unusable tool", names);

    const missing = await server.request("tools/call", {
      name: "create_issue",
      arguments: {},
    });
    check(
      missing.error?.code === -32602,
      "calling a tool the server does not have is a protocol error",
      missing.error,
    );
  } finally {
    server.stop();
  }
}

async function checkArgumentValidation() {
  process.stdout.write("missing required arguments are refused\n");
  const server = startServer();
  try {
    await server.request("initialize", {});
    const response = await server.request("tools/call", {
      name: "create_issue",
      arguments: { title: "no body here" },
    });
    check(response.error?.code === -32602, "a missing required argument is rejected", response.error);
    check(
      String(response.error?.message ?? "").includes("body"),
      "...and the error names the argument, so a mapping bug is diagnosable",
      response.error?.message,
    );
  } finally {
    server.stop();
  }
}

// --- run --------------------------------------------------------------------

const suites = [
  checkHandshakeAndDiscovery,
  checkToolNamesAreAProfile,
  checkJournalRecordsArgsAndOrder,
  checkReadBeforeWrite,
  checkInducedFailures,
  checkUnmappableProfile,
  checkArgumentValidation,
];

for (const suite of suites) {
  await suite();
}

process.stdout.write(`\n${checks - failures}/${checks} checks passed\n`);
process.exit(failures === 0 ? 0 : 1);
