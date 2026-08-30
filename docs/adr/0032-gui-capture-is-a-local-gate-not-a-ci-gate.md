# 0032 — GUI capture is a local gate, not a CI gate

**Status:** accepted
**Context:** SPEC §16.3, §16.4; RL-1104

## The question

The desktop UI is verified by capturing a settled window and reading the PNG
(§16.4). Should that run in CI on every push, or only on a developer's machine?

## What was found

**macOS: not viable on a hosted runner.** Screen capture requires Screen Recording
consent, which macOS grants per-application through a GUI prompt answered by a
logged-in user. A hosted runner has nobody to answer it and no supported way to
pre-seed the grant. This is not a configuration problem; it is the security model
working as designed.

**Linux: viable, and expensive in a way that compounds.** Xvfb can host the window.
The obstacle is upstream of capture: building the shell at all needs webkit2gtk and
around a dozen system libraries. That is an install on every Linux run, forever,
for a check that only one of three platforms can perform.

**Windows: the most likely of the three.** GitHub's Windows images run GUI
applications, and the shell now compiles and starts there — CI launches it and
waits for it to report a window (RL-1101).

## The decision

**Capture is a local developer gate. CI runs everything else.**

CI does: the `vitest` component layer, compiles the shell on macOS and Windows, and
smoke-tests that it starts and creates a window. Visual verification happens where
a real desktop session exists.

## Why not gate on the one platform that could

Gating on Windows alone would make the weakest-supported platform the arbiter of
how the UI looks everywhere. A capture is a picture of *one* renderer: WebView2 on
Windows, WKWebView on macOS, WebKitGTK on Linux. A green Windows capture says
nothing about the other two, and treating it as though it did would be worse than
not gating — it would be a gate that reports on something other than what it
claims.

## Why not accept the Linux cost

It would buy capture on the one platform whose shell CI does not build. Paying a
per-run install to enable a check that then still cannot run anywhere it matters is
the wrong order: build Linux first if Linux matters, and revisit capture after.

## What this costs us

The thing §16.4 exists to prevent — a screen that renders blank and passes every
assertion — is now caught by a person rather than by a machine. That is a real
loss, and it is the reason this ADR exists rather than the decision being implicit.

Two things reduce it. The smoke test catches the whole-window failure (nothing
renders at all) on Windows automatically. And `vitest` covers component logic, so
what capture uniquely catches is narrowed to layout and paint.

## What would change this

A hosted macOS runner able to grant Screen Recording non-interactively, or a
decision to build the shell on Linux for its own sake — at which point capture
there is nearly free and the argument above is reversed.
