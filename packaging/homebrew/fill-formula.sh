#!/usr/bin/env bash
# Prints the Homebrew formula for one release: the template next to this
# script with the version and the three archive checksums filled in.
#
#   packaging/homebrew/fill-formula.sh 0.1.0 path/to/SHA256SUMS > whatsup.rb
#
# SHA256SUMS is the file attached to the GitHub Release (sha256sum format:
# "<64 hex>  <archive name>"). Fails if a checksum is missing, malformed or
# a placeholder is left over.
set -euo pipefail

if [ $# -ne 2 ]; then
  echo "usage: $0 <version, e.g. 0.1.0> <SHA256SUMS>" >&2
  exit 2
fi
version=${1#v}
sums=$2
template="$(cd "$(dirname "$0")" && pwd)/whatsup.rb"

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: '$1' is not a version like 0.1.0" >&2
  exit 2
fi

# sha <target>: the checksum of whatsup-v<version>-<target>.tar.gz.
sha() {
  local file="whatsup-v${version}-$1.tar.gz" sum
  sum=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$sums")
  if ! [[ "$sum" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: no SHA-256 for $file in $sums" >&2
    exit 1
  fi
  printf '%s' "$sum"
}

arm_mac=$(sha aarch64-apple-darwin)
intel_mac=$(sha x86_64-apple-darwin)
linux=$(sha x86_64-unknown-linux-gnu)

out=$(sed \
  -e "s/@VERSION@/${version}/g" \
  -e "s/@SHA256_AARCH64_APPLE_DARWIN@/${arm_mac}/" \
  -e "s/@SHA256_X86_64_APPLE_DARWIN@/${intel_mac}/" \
  -e "s/@SHA256_X86_64_UNKNOWN_LINUX_GNU@/${linux}/" \
  -e '1,4d' \
  "$template")

if grep -q '@[A-Z0-9_]*@' <<<"$out"; then
  echo "error: unfilled placeholder in the formula" >&2
  exit 1
fi
printf '%s\n' "$out"
