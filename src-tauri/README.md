# src-tauri

Placeholder for the Tauri v2 desktop shell (SPEC §4.1, decision D1).

The shell is deliberately thin: window, tray and notifications only. Every IPC
command delegates to `revlocal-daemon`, which runs in-process. Scaffolded here by
`RL-101`; the shell itself is built in `RL-1101`.
