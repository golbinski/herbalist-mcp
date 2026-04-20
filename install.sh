#!/usr/bin/env sh
# herbalist-mcp installer — macOS and Linux
# Usage: curl -fsSL https://raw.githubusercontent.com/golbinski/herbalist-mcp/main/install.sh | sh
set -e

REPO="golbinski/herbalist-mcp"
BINARY="herbalist-mcp"
INSTALL_DIR="${HOME}/.local/bin"

# ── helpers ───────────────────────────────────────────────────────────────────

bold()  { printf '\033[1m%s\033[0m\n'  "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
warn()  { printf '\033[33mwarn:\033[0m  %s\n' "$*" >&2; }
die()   { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

# ── platform detection ────────────────────────────────────────────────────────

detect_artifact() {
    OS=$(uname -s)
    ARCH=$(uname -m)
    case "${OS}-${ARCH}" in
        Darwin-arm64|Darwin-aarch64) echo "herbalist-mcp-macos-aarch64"  ;;
        Darwin-x86_64)               echo "herbalist-mcp-macos-x86_64"   ;;
        Linux-x86_64)                echo "herbalist-mcp-linux-x86_64"   ;;
        *) die "No pre-built binary for ${OS} ${ARCH}. Build from source: cargo build --release" ;;
    esac
}

# ── download and verify ───────────────────────────────────────────────────────

latest_version() {
    need_cmd curl
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' \
        | sed 's/.*"tag_name": *"v\([^"]*\)".*/\1/'
}

download_and_verify() {
    ARTIFACT="$1"
    VERSION="$2"
    DEST="$3"
    BASE="https://github.com/${REPO}/releases/download/v${VERSION}"

    bold "Downloading ${ARTIFACT} v${VERSION}..."
    curl -fsSL --progress-bar -o "$DEST"           "${BASE}/${ARTIFACT}"
    curl -fsSL                -o "${DEST}.sha256"  "${BASE}/${ARTIFACT}.sha256"

    EXPECTED=$(awk '{print $1}' "${DEST}.sha256")
    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL=$(sha256sum  "$DEST" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$DEST" | awk '{print $1}')
    else
        warn "Cannot verify checksum: sha256sum/shasum not found"
        rm -f "${DEST}.sha256"
        return
    fi

    [ "$EXPECTED" = "$ACTUAL" ] || die "SHA256 mismatch!\n  expected: ${EXPECTED}\n  actual:   ${ACTUAL}"
    green "Checksum verified."
    rm -f "${DEST}.sha256"
}

# ── install binary ────────────────────────────────────────────────────────────

install_binary() {
    ARTIFACT=$(detect_artifact)
    VERSION=$(latest_version)
    TMP=$(mktemp)
    download_and_verify "$ARTIFACT" "$VERSION" "$TMP"

    mkdir -p "$INSTALL_DIR"
    mv "$TMP" "${INSTALL_DIR}/${BINARY}"
    chmod +x  "${INSTALL_DIR}/${BINARY}"
    green "Installed to ${INSTALL_DIR}/${BINARY}"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *) warn "${INSTALL_DIR} is not in your PATH. Add it to your shell profile." ;;
    esac
}

# ── JSON config helpers (requires python3) ────────────────────────────────────

have_python() { command -v python3 >/dev/null 2>&1; }

# Merge a single key into mcpServers in a Claude Code JSON config.
write_claude_entry() {
    CONFIG="$1"
    BIN="$2"
    VAULT="$3"
    python3 - "$CONFIG" "$BIN" "$VAULT" <<'EOF'
import json, sys, os

config_path, bin_path, vault = sys.argv[1], sys.argv[2], sys.argv[3]
config = {}
if os.path.exists(config_path):
    with open(config_path) as f:
        try:
            config = json.load(f)
        except json.JSONDecodeError:
            pass

config.setdefault("mcpServers", {})["herbalist"] = {
    "command": bin_path,
    "args": ["serve"],
    "env": {"HERBALIST_VAULT": vault, "HERBALIST_LOG": "herbalist_mcp=warn"},
}
with open(config_path, "w") as f:
    json.dump(config, f, indent=2)
    f.write("\n")
EOF
}

# Merge a single key into servers in a VS Code mcp.json config.
write_vscode_entry() {
    CONFIG="$1"
    BIN="$2"
    VAULT="$3"
    python3 - "$CONFIG" "$BIN" "$VAULT" <<'EOF'
import json, sys, os

config_path, bin_path, vault = sys.argv[1], sys.argv[2], sys.argv[3]
config = {}
if os.path.exists(config_path):
    with open(config_path) as f:
        try:
            config = json.load(f)
        except json.JSONDecodeError:
            pass

config.setdefault("servers", {})["herbalist"] = {
    "type": "stdio",
    "command": bin_path,
    "args": ["serve"],
    "env": {"HERBALIST_VAULT": vault, "HERBALIST_LOG": "herbalist_mcp=warn"},
}
with open(config_path, "w") as f:
    json.dump(config, f, indent=2)
    f.write("\n")
EOF
}

# ── MCP config writers ────────────────────────────────────────────────────────

configure_claude() {
    BIN="$1"
    VAULT="$2"
    CONFIG="${HOME}/.claude.json"

    # Only configure if Claude Code is present or already has a config file
    if ! command -v claude >/dev/null 2>&1 && [ ! -f "$CONFIG" ]; then
        return
    fi

    if ! have_python; then
        warn "python3 not found — skipping Claude Code config. Add manually to ${CONFIG}"
        return
    fi

    bold "Configuring Claude Code..."
    write_claude_entry "$CONFIG" "$BIN" "$VAULT"
    green "  -> ${CONFIG}"
}

configure_vscode() {
    BIN="$1"
    VAULT="$2"

    OS=$(uname -s)
    case "$OS" in
        Darwin) VSCODE_USER="${HOME}/Library/Application Support/Code/User" ;;
        Linux)  VSCODE_USER="${HOME}/.config/Code/User" ;;
        *) return ;;
    esac

    [ -d "$VSCODE_USER" ] || return

    if ! have_python; then
        warn "python3 not found — skipping VS Code config. Add manually to ${VSCODE_USER}/mcp.json"
        return
    fi

    bold "Configuring VS Code MCP..."
    write_vscode_entry "${VSCODE_USER}/mcp.json" "$BIN" "$VAULT"
    green "  -> ${VSCODE_USER}/mcp.json"
}

# ── main ──────────────────────────────────────────────────────────────────────

main() {
    bold "herbalist-mcp installer"
    echo ""

    install_binary

    echo ""
    printf 'Vault path (leave blank to configure later): '
    read -r VAULT

    if [ -n "$VAULT" ]; then
        bold "Indexing vault..."
        "${INSTALL_DIR}/${BINARY}" index --vault "$VAULT"
        configure_claude  "${INSTALL_DIR}/${BINARY}" "$VAULT"
        configure_vscode  "${INSTALL_DIR}/${BINARY}" "$VAULT"
    else
        warn "Skipping index and MCP config — no vault path given."
        echo  "Run the following when ready:"
        echo  "  herbalist-mcp index --vault <path>"
    fi

    echo ""
    green "Done! Run 'herbalist-mcp --help' to get started."
}

main "$@"
