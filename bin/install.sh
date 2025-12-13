#!/bin/sh
set -e

GITHUB_REPO="acidtib/jiji"
INSTALL_DIR=${INSTALL_DIR:-/usr/local/bin}
# Use the latest version or specify the version to install:
#   curl ... | VERSION=v1.2.3 sh
VERSION=${VERSION:-latest}

print_manual_install() {
    RELEASES_URL="https://github.com/${GITHUB_REPO}/releases/${VERSION}"
    echo "Failed while attempting to install jiji. You can install it manually:"
    echo "  1. Open your web browser and go to ${RELEASES_URL}"
    echo "  2. Download jiji-<OS>-<ARCH> for your platform (OS: linux/macos, ARCH: x86_64/arm64)."
    echo "  3. Make the binary executable: chmod +x ./jiji-*"
    echo "  4. Install the binary to /usr/local/bin: sudo install ./jiji-* ${INSTALL_DIR}/jiji"
    echo "  5. Delete the downloaded binary: rm jiji-*"
    echo "  6. Run 'jiji --help' to verify the installation. Enjoy!"
}

fetch_latest_version() {
    latest_url="https://github.com/${GITHUB_REPO}/releases/latest"
    VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$latest_url" | grep -o 'tag/[^/]*$' | cut -d'/' -f2)
    if [ -z "$VERSION" ]; then
        echo "Failed to fetch the latest version from GitHub."
        print_manual_install
        exit 1
    fi
}

# Check if not running as root and need to use sudo to write to INSTALL_DIR.
SUDO=""
if [ "$(id -u)" != "0" ] && [ ! -w "$INSTALL_DIR" ]; then
    if ! command -v sudo >/dev/null 2>&1; then
        echo "Please run this script as root or install sudo."
        print_manual_install
        exit 1
    fi
    SUDO="sudo"
fi

# Detect the user OS and architecture.
OS=$(uname -s)
ARCH=$(uname -m)
case "$OS" in
    Darwin) BINARY_OS="macos" ;;
    Linux)  BINARY_OS="linux" ;;
    *)
        echo "There is no jiji support for $OS/$ARCH. Please open a GitHub issue if you would like to request support."
        exit 1
        ;;
esac
case "$ARCH" in
    aarch64 | arm64) BINARY_ARCH="arm64" ;;
    x86_64)          BINARY_ARCH="x86_64" ;;
    *)
        echo "There is no jiji support for $OS/$ARCH. Please open a GitHub issue if you would like to request support."
        exit 1
        ;;
esac

# Use the latest version if not specified explicitly.
if [ "$VERSION" = "latest" ]; then
    fetch_latest_version
fi

BINARY_NAME="jiji-${BINARY_OS}-${BINARY_ARCH}"

BINARY_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${BINARY_NAME}"

# Create a temporary directory for downloads.
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

# Download the binary.
echo "Downloading jiji binary ${VERSION} ${BINARY_URL}"
if ! curl -fsSL "$BINARY_URL" -o "${TMP_DIR}/${BINARY_NAME}"; then
    echo "Failed to download jiji binary from ${BINARY_URL}"
    print_manual_install
    exit 1
fi
echo "Download complete."

# Make the binary executable.
chmod +x "${TMP_DIR}/${BINARY_NAME}"

# Install the binary.
if [ -z "${SUDO}" ]; then
    echo "Installing jiji binary to ${INSTALL_DIR}"
else
    echo "Installing jiji binary to ${INSTALL_DIR} using sudo. You may be prompted for your password."
fi

if ! $SUDO install "${TMP_DIR}/${BINARY_NAME}" "${INSTALL_DIR}/jiji"; then
    echo "Failed to install jiji binary to ${INSTALL_DIR}"
    print_manual_install
    exit 1
fi

echo "Successfully installed jiji binary ${VERSION} to ${INSTALL_DIR}/jiji"
echo "Run 'jiji --help' to get started"
