## Installing

### macOS (Apple Silicon)

1. Download `Wu-aarch64.dmg`, open it, and drag Wu into your Applications folder. Wu is not signed with an Apple Developer certificate yet, so macOS will block it the first time you open it.
2. Open Terminal and run:

   ```sh
   xattr -d com.apple.quarantine /Applications/Wu.app
   ```

3. Open Wu normally.

If you'd rather not use Terminal: open Wu once (you'll see a "Wu can't be opened" or "Apple could not verify" message), then go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway** next to Wu. Confirm with your password.

### Linux (x86_64 and aarch64)

Download `wu-linux-<arch>.tar.gz` and unpack it into `~/.local`:

```sh
tar -xzf wu-linux-$(uname -m).tar.gz -C ~/.local
ln -sf ~/.local/wu.app/bin/wu ~/.local/bin/wu
```

Make sure `~/.local/bin` is on your `PATH`, then run `wu`.

### Windows (x86_64)

Download and run `Wu-x86_64.exe`. The installer isn't code-signed, so Windows SmartScreen may warn you. Click **More info**, then **Run anyway**.

The `wu-remote-server-*` files are used by Wu's remote development feature. You don't need to download them yourself.
