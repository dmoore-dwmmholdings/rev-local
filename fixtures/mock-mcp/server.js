#!/usr/bin/env node
// A stand-in MCP server over stdio (SPEC §16.2, §11.2).
//
// Three things make this worth having over a hand-rolled stub in each test:
//
//   * TOOL NAMES ARE A PROFILE, not hardcoded. §11.2's whole claim is that the
//     Andare integration does not require knowing Andare's tool names at build
//     time — capability mapping resolves `tool_candidates` against the names the
//     server actually reports. That claim can only be tested against a server
//     willing to report different names, so `profiles/andare-renamed.json` exposes
//     `create_work_item` rather than `create_issue`.
//
//   * EVERY REQUEST IS JOURNALLED, in order. §11.5's read-before-write rule is a
//     statement about the ORDER of two calls, and an assertion on a return value
//     cannot see order. The journal is what makes it checkable.
//
//   * FAILURES ARE ON DEMAND. Rate limits and 5xx are the cases retry policy
//     exists for, and they are exactly what a real server will not produce when
//     asked nicely.
//
// Environment:
//   MOCK_MCP_PROFILE     path to a profile JSON  (default: profiles/default.json)
//   MOCK_MCP_JOURNAL     path to write the JSONL journal to (default: none)
//   MOCK_MCP_FAIL_MODE   rate_limit | server_error | invalid_params | none
//   MOCK_MCP_FAIL_TIMES  fail this many calls, then succeed (default: always)
//   MOCK_MCP_FAIL_TOOL   restrict induced failures to one tool name
//   MOCK_MCP_IGNORE_EOF  keep running after stdin closes, and ignore SIGTERM
//
// MOCK_MCP_IGNORE_EOF exists for the same reason the mock engine's `hang` mode
// does. A well-behaved server exits by itself when its stdin closes, so a client
// that reaps NOTHING still passes a "the process is gone afterwards" test — the
// server left on its own and the client took the credit. This mode is a server
// that will not leave, so the client has to actually kill it. Without it, RL-601's
// "reaped, not leaked, on drop" criterion is untested.

import { appendFileSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";

const HERE = dirname(fileURLToPath(import.meta.url));
const PROTOCOL_VERSION = "2025-06-18";

const profilePath = process.env.MOCK_MCP_PROFILE
  ? resolve(process.env.MOCK_MCP_PROFILE)
  : join(HERE, "profiles", "default.json");
const profile = JSON.parse(readFileSync(profilePath, "utf8"));

const journalPath = process.env.MOCK_MCP_JOURNAL ?? null;
const failMode = process.env.MOCK_MCP_FAIL_MODE ?? "none";
const failTool = process.env.MOCK_MCP_FAIL_TOOL ?? null;
const failTimes =
  process.env.MOCK_MCP_FAIL_TIMES === undefined
    ? Infinity
    : Number(process.env.MOCK_MCP_FAIL_TIMES);

let sequence = 0;
let inducedFailures = 0;

// Pages a get_page has been seen for. Read-before-write is per page, not global:
// reading page A does not license writing page B, and a rule that let it would be
// satisfied by any single read at startup.
const pagesRead = new Set();

/** Append one entry to the journal. No timestamps — tests assert on order. */
function journal(entry) {
  sequence += 1;
  const line = JSON.stringify({ seq: sequence, ...entry });
  if (journalPath) {
    appendFileSync(journalPath, `${line}\n`, "utf8");
  }
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function reply(id, result) {
  send({ jsonrpc: "2.0", id, result });
}

function replyError(id, code, message, data) {
  send({ jsonrpc: "2.0", id, error: { code, message, ...(data ? { data } : {}) } });
}

/** A tool result in MCP's content shape. */
function toolText(text) {
  return { content: [{ type: "text", text }], isError: false };
}

/** A tool-level error: the call completed, the tool refused. */
function toolError(text) {
  return { content: [{ type: "text", text }], isError: true };
}

function pageKey(args) {
  return `${args.space ?? ""}/${args.title ?? ""}`;
}

/** Should this call be turned into an induced failure? */
function shouldFail(toolName) {
  if (failMode === "none") return false;
  if (failTool && failTool !== toolName) return false;
  return inducedFailures < failTimes;
}

function induceFailure(id, toolName) {
  inducedFailures += 1;
  journal({ kind: "induced_failure", tool: toolName, mode: failMode });

  switch (failMode) {
    case "rate_limit":
      // Retryable. `retry_after_ms` is what a backoff policy should honour rather
      // than guessing.
      return replyError(id, -32001, "rate limited", {
        retryable: true,
        retry_after_ms: 250,
        http_status: 429,
      });
    case "server_error":
      return replyError(id, -32002, "upstream returned 500", {
        retryable: true,
        http_status: 503,
      });
    case "invalid_params":
      // NOT retryable. A retry policy that backs off on this would loop until it
      // gave up, turning a caller bug into a slow failure.
      return replyError(id, -32602, "invalid params", { retryable: false });
    default:
      return replyError(id, -32603, `unknown MOCK_MCP_FAIL_MODE: ${failMode}`);
  }
}

function callTool(id, params) {
  const name = params?.name;
  const args = params?.arguments ?? {};
  const tool = profile.tools.find((t) => t.name === name);

  journal({ kind: "tools/call", tool: name, args });

  if (!tool) {
    // A tool that does not exist is a protocol-level error, distinct from a tool
    // that ran and refused.
    return replyError(id, -32602, `unknown tool: ${name}`, { retryable: false });
  }

  if (shouldFail(name)) {
    return induceFailure(id, name);
  }

  // Required-argument checking, so a test can tell "the mapper rendered the wrong
  // args" from "the tool refused for its own reasons".
  const required = tool.inputSchema?.required ?? [];
  const missing = required.filter((key) => args[key] === undefined);
  if (missing.length > 0) {
    return replyError(id, -32602, `missing required arguments: ${missing.join(", ")}`, {
      retryable: false,
    });
  }

  switch (tool.behavior) {
    case "records_read":
      pagesRead.add(pageKey(args));
      return reply(
        id,
        toolText(`# ${args.title}\n\nExisting body of ${pageKey(args)}.\n`),
      );

    case "read_before_write": {
      // SPEC §11.5, and the reason this fixture exists. update_page REPLACES a
      // body, so writing without having read first silently destroys whatever was
      // there. Refusing here is what makes that a testable rule rather than a
      // convention.
      if (!pagesRead.has(pageKey(args))) {
        journal({ kind: "read_before_write_violation", tool: name, page: pageKey(args) });
        return reply(
          id,
          toolError(
            `refusing to update ${pageKey(args)}: no get_page was called for it first. ` +
              `update_page replaces the body, so writing blind destroys the existing page.`,
          ),
        );
      }
      return reply(id, toolText(`updated ${pageKey(args)}`));
    }

    case "ok":
    default:
      return reply(id, toolText(tool.result ?? `${name} ok`));
  }
}

function handle(message) {
  const { id, method, params } = message;

  switch (method) {
    case "initialize":
      journal({ kind: "initialize", args: params ?? {} });
      return reply(id, {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: `mock-mcp/${profile.name}`, version: "1.0.0" },
        ...(profile.instructions ? { instructions: profile.instructions } : {}),
      });

    case "notifications/initialized":
      journal({ kind: "notifications/initialized" });
      return; // a notification has no id and takes no reply

    case "tools/list":
      journal({ kind: "tools/list" });
      return reply(id, {
        tools: profile.tools.map((tool) => ({
          name: tool.name,
          description: tool.description ?? "",
          inputSchema: tool.inputSchema ?? { type: "object" },
        })),
      });

    case "tools/call":
      return callTool(id, params);

    case "ping":
      return reply(id, {});

    default:
      journal({ kind: "unknown_method", method });
      if (id !== undefined) {
        replyError(id, -32601, `method not found: ${method}`);
      }
  }
}

// See the header: a server that refuses to exit, so a client's reaping is testable.
if (process.env.MOCK_MCP_IGNORE_EOF === "1") {
  process.on("SIGTERM", () => {});
  process.on("SIGINT", () => {});
  // Something on the event loop forever, so closing stdin does not end the process.
  setInterval(() => {}, 1000);
}

const lines = createInterface({ input: process.stdin, terminal: false });
lines.on("line", (line) => {
  const text = line.trim();
  if (text === "") return;

  let message;
  try {
    message = JSON.parse(text);
  } catch (err) {
    // No id to reply against, so this is reported the only way it can be.
    send({ jsonrpc: "2.0", id: null, error: { code: -32700, message: `parse error: ${err}` } });
    return;
  }

  try {
    handle(message);
  } catch (err) {
    if (message?.id !== undefined) {
      replyError(message.id, -32603, `mock-mcp internal error: ${err?.stack ?? err}`);
    }
  }
});
