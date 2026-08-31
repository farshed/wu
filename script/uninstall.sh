#!/usr/bin/env sh
set -eu

# Uninstalls Wu that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        # Check for any Wu variants in /Applications
        remaining=$(ls -d /Applications/Wu*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        # Check for any Wu variants in ~/.local
        remaining=$(ls -d "$HOME/.local/wu"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Wu preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/wu"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${ZED_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "Wu has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    db_suffix="stable"
    case "$channel" in
      stable)
        appid="me.farshed.Wu"
        db_suffix="stable"
        ;;
      dev)
        appid="me.farshed.Wu-Dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="me.farshed.Wu"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/wu$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/wu"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/wu/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/wu/wu-$db_suffix.sock"

    # Remove the entire Wu directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/wu"
        prompt_remove_preferences
    fi

    rm -rf "$HOME/.wu_server"
}

macos() {
    app="Wu.app"
    db_suffix="stable"
    app_id="me.farshed.Wu"
    case "$channel" in
      dev)
        app="Wu Dev.app"
        db_suffix="dev"
        app_id="me.farshed.Wu-Dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/wu"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Wu/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Wu directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Wu"
        rm -rf "$HOME/Library/Logs/Wu"

        prompt_remove_preferences
    fi

    rm -rf "$HOME/.wu_server"
}

main "$@"
