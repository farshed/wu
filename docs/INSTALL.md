## Install

All releases are available [here](https://github.com/farshed/wu/releases/latest).

### macOS (Apple Silicon)

1. Download the [installer](https://github.com/farshed/wu/releases/latest/download/Wu-aarch64.dmg), open it, and drag Wu into your Applications folder. Wu is not signed with an Apple Developer certificate yet, so macOS will block it the first time you open it.
2. Open Terminal and run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Wu.app
   ```

> If you'd rather not use Terminal: open Wu once (you'll see a "Wu can't be opened" or "Apple could not verify" message), then go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway** next to Wu. Confirm with your password.

3. Open Wu normally.

### Linux (x86-64 and AArch64)

1. Download the tarball for your platform.
    - [**Linux x86-64**](https://github.com/farshed/wu/releases/latest/download/wu-linux-x86_64.tar.gz)
    - [**Linux AArch64**](https://github.com/farshed/wu/releases/latest/download/wu-linux-aarch64.tar.gz)

2. Unpack its contents into `~/.local`:

```sh
tar -xzf wu-linux-$(uname -m).tar.gz -C ~/.local
ln -sf ~/.local/wu.app/bin/wu ~/.local/bin/wu
```

Make sure `~/.local/bin` is on your `PATH`, then run `wu`.

### Windows (x86-64)

Download the [installer](https://github.com/farshed/wu/releases/latest/download/Wu-x86_64.exe) and click to run. The installer isn't code-signed, so Windows SmartScreen may warn you. Click **More info**, then **Run anyway**.
