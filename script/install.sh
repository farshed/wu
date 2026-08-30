#!/usr/bin/env sh
set -eu

# Downloads a Wu release from GitHub and unpacks it into ~/.local/.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${ZED_CHANNEL:-stable}"
    repo="farshed/wu"
    ZED_VERSION="${ZED_VERSION:-latest}"
    if [ "$ZED_VERSION" = "latest" ]; then
        download_base="https://github.com/$repo/releases/latest/download"
    else
        download_base="https://github.com/$repo/releases/download/v$ZED_VERSION"
    fi
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/wu-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/wu-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v wu)" = "$HOME/.local/bin/wu" ]; then
        echo "Wu has been installed. Run with 'wu'"
    else
        echo "To run Wu from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run Wu now, '~/.local/bin/wu'"
    fi
}

linux() {
    if [ -n "${ZED_BUNDLE_PATH:-}" ]; then
        cp "$ZED_BUNDLE_PATH" "$temp/wu-linux-$arch.tar.gz"
    else
        echo "Downloading Wu version: $ZED_VERSION"
        curl "$download_base/wu-linux-$arch.tar.gz" > "$temp/wu-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="me.farshed.Wu"
        ;;
      dev)
        appid="me.farshed.Wu-Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="me.farshed.Wu"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/wu$suffix.app"
    mkdir -p "$HOME/.local/wu$suffix.app"
    tar -xzf "$temp/wu-linux-$arch.tar.gz" -C "$HOME/.local/"

    zed_editor="$HOME/.local/wu$suffix.app/libexec/wu-editor"
    if [ -f "$zed_editor" ] && command -v ldd >/dev/null 2>&1; then
        missing="$(ldd "$zed_editor" 2>/dev/null | sed -n 's/^[[:space:]]*\(.*\) => not found$/\1/p')"
        if [ -n "$missing" ]; then
            echo "Warning: your system is missing libraries that Wu needs:"
            echo "$missing" | sed 's/^/    /'
            echo "Install them with your package manager, or Wu will fail to start."
        fi
    fi

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    ln -sf "$HOME/.local/wu$suffix.app/bin/wu" "$HOME/.local/bin/wu"

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/wu$suffix.app/share/applications"
    cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    sed -i "s|Icon=wu|Icon=$HOME/.local/wu$suffix.app/share/icons/hicolor/512x512/apps/wu.png|g" "${desktop_file_path}"
    sed -i "s|Exec=wu|Exec=$HOME/.local/wu$suffix.app/bin/wu|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading Wu version: $ZED_VERSION"
    curl "$download_base/Wu-$arch.dmg" > "$temp/Wu-$arch.dmg"
    hdiutil attach -quiet "$temp/Wu-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/wu"
}

main "$@"
