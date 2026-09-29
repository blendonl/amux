#!/bin/sh
set -eu

say() {
  printf '%s\n' "$*"
}

fail() {
  printf 'amux install: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "this needs $1, and there is no $1 on PATH"
}

fetch() {
  curl --fail --silent --show-error --location --proto-redir =https "$@"
}

host_target() {
  arch=$(uname -m)
  os=$(uname -s)
  case $arch in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "there are no amux releases for $arch machines; build amux from source" ;;
  esac
  if [ "$os" = Darwin ] && [ "$arch" = x86_64 ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
    arch=aarch64
  fi
  case $os in
    Linux) say "$arch-unknown-linux-musl" ;;
    Darwin) say "$arch-apple-darwin" ;;
    *) fail "there are no amux releases for $os; build amux from source" ;;
  esac
}

latest_tag() {
  json=$(fetch --header 'Accept: application/vnd.github+json' "$1") ||
    fail "could not find the latest amux release at $1"
  tag=$(printf '%s\n' "$json" |
    sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
    head -n 1)
  [ -n "$tag" ] || fail "$1 does not name a release"
  say "$tag"
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    fail "this needs sha256sum or shasum to check the download"
  fi
}

main() {
  need curl
  need tar
  need uname

  if [ -n "${AMUX_RELEASES_URL:-}" ]; then
    releases=${AMUX_RELEASES_URL%/}
    latest=$releases/latest
    download=$releases/download
  else
    latest=https://api.github.com/repos/blendonl/amux/releases/latest
    download=https://github.com/blendonl/amux/releases/download
  fi
  install_dir=${AMUX_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}

  target=$(host_target)
  if [ -n "${AMUX_VERSION:-}" ]; then
    tag=v${AMUX_VERSION#v}
  else
    tag=$(latest_tag "$latest")
  fi
  package=amux-$target
  archive=$package.tar.gz

  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t amux)
  trap 'rm -rf "$tmp"' EXIT
  trap 'exit 1' HUP INT TERM

  say "downloading amux ${tag#v} for $target"
  fetch --output "$tmp/$archive" "$download/$tag/$archive" ||
    fail "could not download $download/$tag/$archive"
  fetch --output "$tmp/$archive.sha256" "$download/$tag/$archive.sha256" ||
    fail "could not download $download/$tag/$archive.sha256"

  read -r expected _ <"$tmp/$archive.sha256" || true
  actual=$(sha256 "$tmp/$archive")
  [ "$(printf '%s' "${expected:-}" | tr 'A-F' 'a-f')" = "$actual" ] ||
    fail "$archive does not match its checksum, so nothing was installed"

  tar -xzf "$tmp/$archive" -C "$tmp" || fail "could not unpack $archive"
  binary=$tmp/$package/amux
  [ -f "$binary" ] || fail "$archive has no $package/amux"
  "$binary" --version >/dev/null 2>&1 ||
    fail "the amux ${tag#v} download does not run on this machine"

  mkdir -p "$install_dir" || fail "could not create $install_dir"
  staged=$install_dir/.amux-install.$$
  cp "$binary" "$staged" || fail "could not write to $install_dir"
  chmod 755 "$staged"
  mv -f "$staged" "$install_dir/amux"

  say "installed $("$install_dir/amux" --version) in $install_dir"
  case ":${PATH:-}:" in
    *":$install_dir:"*)
      found=$(command -v amux || true)
      if [ -n "$found" ] && [ "$found" != "$install_dir/amux" ]; then
        say "$found comes before it on your PATH"
      fi
      ;;
    *)
      say "$install_dir is not on your PATH; add it in your shell profile:"
      say "  export PATH=\"$install_dir:\$PATH\""
      ;;
  esac
  say "run \`amux update\` to update it later"
}

main "$@"
