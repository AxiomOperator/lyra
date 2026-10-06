#!/bin/sh
# Install lyra-node: let lyra work on this machine.
#   curl -fsSL __LYRA_URL__/install.sh | sh -s -- [--name NAME]
# Downloads the static lyra-node (x86_64 Linux), checks it against the server's
# checksum, installs it (root: /usr/local/bin, else ~/.local/bin), asks lyra to
# pair (approve it in the web app or a lyra terminal) and starts its service.
set -eu
URL="__LYRA_URL__"
NAME=""
while [ $# -gt 0 ]; do
  case "$1" in
    --name) NAME="$2"; shift 2 ;;
    *) shift ;;
  esac
done
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) ;;
  *) echo "lyra-node: only x86_64 Linux is supported (this is $(uname -s) $(uname -m))" >&2; exit 1 ;;
esac
command -v curl >/dev/null 2>&1 || { echo "lyra-node: curl is needed" >&2; exit 1; }
if [ "$(id -u)" = 0 ]; then DIR=/usr/local/bin; else DIR="$HOME/.local/bin"; fi
mkdir -p "$DIR"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
echo "downloading lyra-node from $URL"
curl -fsSL "$URL/download/lyra-node" -o "$TMP"
WANT="$(curl -fsSL "$URL/download/lyra-node.sha256" | cut -d' ' -f1)"
GOT="$(sha256sum "$TMP" | cut -d' ' -f1)"
if [ "$WANT" != "$GOT" ]; then
  echo "lyra-node: the download doesn't match the server's checksum; not installing" >&2
  exit 1
fi
chmod 755 "$TMP"
mv "$TMP" "$DIR/lyra-node"
trap - EXIT
command -v restorecon >/dev/null 2>&1 && restorecon "$DIR/lyra-node" >/dev/null 2>&1 || true
echo "installed $DIR/lyra-node"
if [ -n "$NAME" ]; then
  "$DIR/lyra-node" pair "$URL" --name "$NAME"
else
  "$DIR/lyra-node" pair "$URL"
fi
"$DIR/lyra-node" service --enable
