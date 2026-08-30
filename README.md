<div align="center">
  <img src="crates/wu/resources/app-icon.png" alt="Wu" width="128">
  <h1>Wu</h1>
  <p>The fast, native code editor that doesn't get in your way.</p>
  <p><a href="https://github.com/farshed/wu/releases/latest"><strong>Download</strong></a></p>
</div>

---

Wu is a code editor for people who want the speed of a native app and the familiarity of VS Code. The name comes from [wu wei](https://en.wikipedia.org/wiki/Wu_wei): effortless action. The editor should do its job and stay out of the way.

Wu is built on the [Zed](https://github.com/zed-industries/zed) engine, so it inherits Zed's editor core, GPU rendering, and language tooling, then strips out everything that isn't editing.

## Features

- **Native and fast.** Written in Rust, rendered on the GPU. No Electron or webviews. Opens instantly and stays responsive on large files.
- **Lightweight.** Base memory use is about 28% lower than even Zed.
- **No built-in AI features.** Bring whichever agent or harness you already use.
- **Feels like VS Code out of the box.** Layout, panels, and defaults are tuned so you don't have to relearn your editor.

---

![Wu screenshot dark](assets/images/screenshot-dark.png)

![Wu screenshot light](assets/images/screenshot-light.png)

## Installing on macOS

Wu is not signed with an Apple Developer certificate, so macOS will block it the first time you open it. This is a one-time step.

1. Open the `.dmg` and drag Wu into your Applications folder.
2. Open Terminal and run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Wu.app

3. Open Wu normally.

If you'd rather not use Terminal: open Wu once (you'll see a "Wu can't be opened" or "Apple could not verify" message), then go to System Settings → Privacy & Security, scroll down, and click Open Anyway next to Wu. Confirm with your password.

## Docs

See [docs](https://wu.farshed.me).

## Building

Wu builds the same way Zed does. See the [Zed development docs](https://zed.dev/docs/development).

## Licensing

Wu is licensed under GPL-3.0-or-later with Apache-2.0 components where marked. See [LICENSE-GPL](./LICENSE-GPL) and [LICENSE-APACHE](./LICENSE-APACHE).
