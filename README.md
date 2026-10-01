<div align="center">
  <img src="crates/wu/resources/app-icon.png" alt="Wu" width="128">
  <h1>Wu</h1>
  <p>The fast, native code editor that doesn't get in your way.</p>
  <p><a href="#download"><strong>Download</strong></a></p>
</div>

---

Wu is a code editor for people who want the speed of a native app and the familiarity of VS Code. The name comes from [wu wei](https://en.wikipedia.org/wiki/Wu_wei): effortless action.

Wu is a fork of [Zed](https://github.com/zed-industries/zed). It inherits Zed's editor core, GPU rendering, and language tooling, but strips out the collab and AI features.

## Features

- **Native and fast.** Written in Rust. No Electron or webviews. Opens instantly and stays responsive on large views.
- **Lightweight.** Wu's memory footprint is lower than VS Code and Zed. See [benchmarks](https://github.com/farshed/wu/blob/main/docs/memory-benchmark.md).
- **No built-in AI features.** Bring whichever agent or harness you already use.
- **Feels like VS Code out of the box.** UI and defaults are tuned so you don't have to relearn your editor.
- **No account or telemetry.** Nothing to sign in to. Wu never sends your usage data anywhere.

## Download

- [macOS (Apple Silicon)](https://github.com/farshed/wu/releases/latest/download/Wu-aarch64.dmg)
- [Linux x86-64](https://github.com/farshed/wu/releases/latest/download/wu-linux-x86_64.tar.gz)
- [Linux AArch64](https://github.com/farshed/wu/releases/latest/download/wu-linux-aarch64.tar.gz)
- [Windows x86-64](https://github.com/farshed/wu/releases/latest/download/Wu-x86_64.exe)

See [INSTALL.md](https://github.com/farshed/wu/blob/main/docs/INSTALL.md) for more.

## Docs

See [docs](https://wu.farshed.me).

## Raycast

On macOS, the [Wu extension for Raycast](https://www.raycast.com/farshed/wu-editor) lets you open recent projects, new windows, settings and more straight from Raycast.

---

![Wu screenshot dark](https://wu.farshed.me/screenshot-dark.png)

![Wu screenshot light](https://wu.farshed.me/screenshot-light.png)

## Building

Wu builds the same way Zed does. See the [Zed development docs](https://zed.dev/docs/development).

## Licensing

Wu is licensed under GPL-3.0-or-later with Apache-2.0 components where marked. See [LICENSE-GPL](./LICENSE-GPL) and [LICENSE-APACHE](./LICENSE-APACHE).

Wu is a derivative work of [Zed](https://github.com/zed-industries/zed) and shares the same licenses.

## Acknowledgements

Thanks to the Zed team for building an excellent editor and releasing it as open source. Wu would not exist without their work.
