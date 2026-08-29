> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.

# Edna

Edna is a fast, simple code editor. It is a specialized distribution of [Zed](https://github.com/zed-industries/zed), trimmed down to the essentials: editing, terminal, git, debugging, and extensions.

## Docs

[docs/index.html](./docs/index.html) explains how Edna differs from Zed. For everything else, use the [Zed docs](https://zed.dev/docs).

## Building

Same as Zed: see the [Zed development docs](https://zed.dev/docs/development) for macOS, Linux, and Windows. Run `cargo run` to start a dev build.

Edna uses Zed's extension registry, so existing Zed extensions work as-is. Project settings live in `.zed/` like they do in Zed.

## Licensing

Edna is based on Zed, which is licensed under GPL-3.0-or-later with Apache-2.0 components where marked. Edna is distributed under the same terms. See [LICENSE-GPL](./LICENSE-GPL) and [LICENSE-APACHE](./LICENSE-APACHE).

To generate the third-party license notices, run `script/generate-licenses`. If it fails on a dependency's license, add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`, or add a clarification entry there.
