#!/bin/sh
set -e

REPO="namanrjpt/agentick"
INSTALL_DIR="$HOME/.local/bin"
BINARY_NAME="agentick"

# --- Helpers ---

info() {
    printf '\033[1;34m==>\033[0m %s\n' "$1"
}

error() {
    printf '\033[1;31merror:\033[0m %s\n' "$1" >&2
    exit 1
}

# --- Uninstall ---

if [ "${1:-}" = "--uninstall" ]; then
    if [ -f "${INSTALL_DIR}/${BINARY_NAME}" ]; then
        rm -f "${INSTALL_DIR}/${BINARY_NAME}"
        info "Removed ${INSTALL_DIR}/${BINARY_NAME}"
    else
        info "${BINARY_NAME} is not installed at ${INSTALL_DIR}/${BINARY_NAME}"
    fi
    if [ -d "$HOME/.agentick" ]; then
        printf '  Config directory ~/.agentick/ was kept. Remove it manually if desired:\n'
        printf '    rm -rf ~/.agentick\n'
    fi
    exit 0
fi

# --- Preflight checks ---

command -v curl >/dev/null 2>&1 || error "curl is required but not found. Please install curl and try again."

# --- Detect OS ---

OS="$(uname -s)"
case "$OS" in
    Linux)  os="unknown-linux-gnu" ;;
    Darwin) os="apple-darwin" ;;
    *)      error "Unsupported operating system: $OS (only Linux and macOS are supported)" ;;
esac

# --- Detect architecture ---

ARCH="$(uname -m)"
case "$ARCH" in
    x86_64)         arch="x86_64" ;;
    aarch64|arm64)  arch="aarch64" ;;
    *)              error "Unsupported architecture: $ARCH (only x86_64 and aarch64/arm64 are supported)" ;;
esac

TARGET="${arch}-${os}"
ASSET_NAME="agentick-${TARGET}"

info "Detected platform: ${TARGET}"

# --- Fetch latest release ---

info "Fetching latest release from GitHub..."
RELEASE_JSON="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")" \
    || error "Failed to fetch latest release from GitHub. Check your internet connection."

VERSION="$(printf '%s' "$RELEASE_JSON" | grep '"tag_name"' | sed 's/.*"tag_name": *"//;s/".*//')"
if [ -z "$VERSION" ]; then
    error "Could not determine latest release version."
fi

info "Latest version: ${VERSION}"

# --- Find download URL ---

DOWNLOAD_URL="$(printf '%s' "$RELEASE_JSON" | grep '"browser_download_url"' | grep "$ASSET_NAME" | head -1 | sed 's/.*"browser_download_url": *"//;s/".*//')"
if [ -z "$DOWNLOAD_URL" ]; then
    error "No binary found for platform ${TARGET} in release ${VERSION}. Available assets may not include your platform."
fi

# --- Download and install ---

info "Downloading ${ASSET_NAME}..."
TMPFILE="$(mktemp)"
trap 'rm -f "$TMPFILE"' EXIT

curl -fSL --progress-bar -o "$TMPFILE" "$DOWNLOAD_URL" \
    || error "Failed to download binary from ${DOWNLOAD_URL}"

mkdir -p "$INSTALL_DIR"
mv "$TMPFILE" "${INSTALL_DIR}/${BINARY_NAME}"
chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

# --- PATH advice ---

case ":$PATH:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        printf '\n'
        info "NOTE: ${INSTALL_DIR} is not in your PATH."
        printf '  Add it by appending one of the following to your shell profile:\n'
        printf '\n'
        printf '    export PATH="%s:$PATH"\n' "$INSTALL_DIR"
        printf '\n'
        ;;
esac

# --- Done ---

printf '\n'
info "Successfully installed ${BINARY_NAME} ${VERSION} to ${INSTALL_DIR}/${BINARY_NAME}"
