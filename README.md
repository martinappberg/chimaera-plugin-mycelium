# chimaera-plugin-mycelium

Mycelium for Chimaera: the Knowledge provider for workspaces that keep mycelium's project memory (.living/), plus knowledge_search and knowledge_get for every agent.

A [Chimaera](https://github.com/martinappberg/chimaera) workbench plugin: a Rust crate compiled to one portable WebAssembly component (`plugin.wasm`) plus its manifest (`plugin.toml`), run by the daemon's sandboxed plugin host through the `chimaera:plugin` interface. Everything it does goes through host functions the daemon bounds; it cannot open files outside the workspace, run processes or reach the network.

## Install

From a Chimaera daemon: `chimaera plugin add martinappberg/chimaera-plugin-mycelium`, or the Plugins tab. Nothing runs until the plugin is switched on in a workspace. Chimaera itself ships this plugin embedded (pinned in its `plugins/plugins.lock`); a newer release here can be installed over the embedded copy from the Plugins tab.

## Develop

```sh
cargo test                                        # the pure logic, natively
cargo build --release --target wasm32-wasip2     # the component
```

The host functions and the manifest are documented in Chimaera's [plugin authoring guide](https://github.com/martinappberg/chimaera/blob/main/docs/agent-guides/plugins.md).

## Release

Bump `version` in both `Cargo.toml` and `plugin.toml`, commit, and push a tag `vX.Y.Z`. The release workflow builds the component and publishes `plugin.wasm`, `plugin.toml` and `SHA256SUMS` on the GitHub release; a daemon's update checker reads this repository's latest release.
