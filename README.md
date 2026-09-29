# chimaera-plugin-mycelium

Mycelium for Chimaera: the Knowledge provider for workspaces that keep mycelium's project memory (.living/), plus knowledge_search and knowledge_get for every agent.

A [Chimaera](https://github.com/martinappberg/chimaera) workbench plugin: a Rust crate compiled to one portable WebAssembly component (`plugin.wasm`) plus its manifest (`plugin.toml`), run by the daemon's sandboxed plugin host through the `chimaera:plugin` interface. Everything it does goes through host functions the daemon bounds; it cannot open files outside the workspace, run processes or reach the network.

## Install

From a Chimaera daemon: `chimaera plugin add martinappberg/chimaera-plugin-mycelium`, or the Plugins tab. Nothing runs until the plugin is switched on in a workspace. Chimaera itself ships this plugin embedded (pinned in its `plugins/plugins.lock`); a newer release here can be installed over the embedded copy from the Plugins tab.

## What it reads

Only what agents write, never computed: `.living/findings/*.md`, `decisions.md`, `learnings.md`, `conventions.md` and `generated-conventions/*/convention.md`, `.living/log/LOG_REGISTRY.md`, `todo/TODO_REGISTRY.md` (the table and the `##` to-do sections below it), and the newest handoff among `.mycelium/last-session.md` and every `.mycelium/run/<host>/<session>/last-session.md`. It reads the shapes real projects drift into as well as mycelium's templates: prose findings led by `**Setup.**`-style labels, `·`-joined fields, follow-up headings (`F-073 CORRECTION`, `F-024 RESOLVED`, `F-026 reprocess round 1`), `### D-157 — title (date)` decisions, and markers like `⛔ SUPERSEDED BY D-125` or `⚠️ SUSPECT`. A finding's `status` is the Mycelium word its Status starts with (else `unknown`); `stated` is the Status as written. Nothing here rates a finding.

## The snapshot

The `knowledge` export returns one JSON snapshot; Chimaera passes it straight to its Knowledge view. The 0.1.3 fields keep their names and meaning, and everything 0.2.0 adds is omitted when empty:

| Field | On | Meaning |
|---|---|---|
| `span {path, line, end_line}` | every entry, follow-up, to-do, ask, the handoff | where its markdown lives (1-based, inclusive); the view renders the slice, so bodies never ride the snapshot |
| `key` | finding, to-do | unique even when ids collide: `topic/F-177`, `topic/F-177~2`, `todo/#50`, `todo/r3` |
| `stated`, `date`, `statement` | finding (`stated` also on decisions and follow-ups) | the Status as written, the entry's date, the `**Claim:**` |
| `refs[] {kind, id}`, `cites[] {kind, text}` | every entry | the ids it cites; the scripts, data, figures, jobs and commits it names |
| `state {kind, by?}`, `amends[] {kind, id}` | finding, decision | superseded / corrected / retracted / suspect / resolved, only from what the text says |
| `addenda[].kind` | finding | addendum / correction / resolution / update |
| `id` | decision, learning, to-do | `D-157`, `L-4` (mycelium's positional id when a file has no explicit ones), `#50`, `T-Name` |
| `title`, `closed`, `source` | to-do | its lead, whether the status says it is closed, `table` or `section` |
| `left_off.sources[]` | handoff | every handoff found, newest first |
| `conventions[]`, `sessions[]` | snapshot | `.living/conventions.md` sections and generated conventions; `LOG_REGISTRY.md` rows |
| `asks[]` | snapshot | sentences that put something to the user, from the handoff and recent findings and decisions |
| `tidy[]` | snapshot | factual inconsistencies (reused ids, to-dos kept outside the registry, a stub handoff, twin to-dos), each with the request an agent would need |
| `id_shapes[]`, `labels`, `guidance[]` | snapshot | the id shapes chats may turn into links, the plugin's words for the view, `MYCELIUM.md` |

The full contract is the "Wire spec" section of Chimaera's `docs/knowledge-redesign-plan.md`. A snapshot over 3.5 MiB (the host refuses 4 MiB) shortens long text fields, then drops cites, then the oldest entries of whichever section is largest until it fits, and says so in `warnings`.

## Develop

```sh
cargo test                                        # the pure logic, natively
cargo build --release --target wasm32-wasip2     # the component
```

`fixtures/reference/` is a synthetic project with every shape the reader handles; its counts are pinned in `src/reader/shapes.rs`. To check the reader against a real project without copying anything into the repository, `MYCELIUM_TREE=/path/to/project cargo test --release probe -- --ignored --nocapture` prints counts only.

The host functions and the manifest are documented in Chimaera's [plugin authoring guide](https://github.com/martinappberg/chimaera/blob/main/docs/agent-guides/plugins.md).

## Release

Bump `version` in both `Cargo.toml` and `plugin.toml`, commit, and push a tag `vX.Y.Z`. The release workflow builds the component and publishes `plugin.wasm`, `plugin.toml` and `SHA256SUMS` on the GitHub release; a daemon's update checker reads this repository's latest release.
