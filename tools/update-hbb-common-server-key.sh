#!/usr/bin/env bash

set -euo pipefail

umask 077

usage() {
  cat >&2 <<'EOF'
Usage: tools/update-hbb-common-server-key.sh [--server HOST[:PORT]] [--config PATH] [--key-file PATH] [--dry-run] [--clean]

Defaults:
  --server   rustdesk.hk.gy
  --config   <repo>/libs/hbb_common/src/config.rs
  --key-file <repo>/pri-key.file
EOF
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'Missing required command: %s\n' "$1" >&2
    exit 1
  }
}

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

server="rustdesk.hk.gy"
config_path="${REPO_ROOT}/libs/hbb_common/src/config.rs"
key_file="${REPO_ROOT}/pri-key.file"
dry_run=0
clean=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)
      [[ $# -ge 2 ]] || {
        printf 'Missing value for %s\n' "$1" >&2
        usage
        exit 1
      }
      server="$2"
      shift 2
      ;;
    --config)
      [[ $# -ge 2 ]] || {
        printf 'Missing value for %s\n' "$1" >&2
        usage
        exit 1
      }
      config_path="$2"
      shift 2
      ;;
    --key-file)
      [[ $# -ge 2 ]] || {
        printf 'Missing value for %s\n' "$1" >&2
        usage
        exit 1
      }
      key_file="$2"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --clean)
      clean=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      usage
      exit 1
      ;;
  esac
done

require_cmd python3
if [[ "${clean}" -eq 0 ]]; then
  require_cmd openssl
fi

tmpdir=""
cleanup() {
  if [[ -n "${tmpdir}" ]]; then
    rm -rf -- "${tmpdir}"
  fi
}
trap cleanup EXIT HUP INT TERM

pem_path=""
if [[ "${clean}" -eq 0 ]]; then
  tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/rustdesk-key.XXXXXX")"
  pem_path="${tmpdir}/private.pem"
  openssl genpkey -algorithm ED25519 -out "${pem_path}" >/dev/null
fi

python3 - "${pem_path}" "${server}" "${config_path}" "${key_file}" "${dry_run}" "${clean}" <<'PY'
import base64
import os
import pathlib
import re
import stat
import subprocess
import sys
import tempfile

ORIGINAL_SERVER = "rs-ny.rustdesk.com"
ORIGINAL_PUB_KEY = "OeVuKk5nlHiXp+APNn0Y3pC1Iwpwn44JGqrQCsWqmBw="


def fail(message):
    print(message, file=sys.stderr)
    raise SystemExit(1)


def bytes_from_labels(txt, labels):
    aliases = {label.lower() for label in labels}
    lines = txt.splitlines()
    for idx, line in enumerate(lines):
        token = line.strip().rstrip(":").lower()
        if not token or not line.strip().endswith(":") or token not in aliases:
            continue
        hex_parts = []
        for next_line in lines[idx + 1 :]:
            seg = next_line.strip().replace(" ", "")
            if not seg:
                continue
            if not re.fullmatch(r"[0-9a-f:]+", seg, flags=re.IGNORECASE):
                break
            hex_parts.append(seg)
        if not hex_parts:
            continue
        data = bytes.fromhex("".join(part.replace(":", "") for part in hex_parts))
        if len(data) not in (32, 64):
            fail(f"unexpected block length for {labels}: {len(data)}")
        if token in {"pub", "public", "public key"} and len(data) == 64:
            return data[:32]
        return data
    fail(f"missing block for {labels}")


def render_replacement(text, pattern, replacement, label):
    matches = list(re.finditer(pattern, text, flags=re.MULTILINE))
    if len(matches) != 1:
        fail(f"expected exactly one {label} match, found {len(matches)}")
    return re.sub(pattern, replacement, text, count=1, flags=re.MULTILINE)


def atomic_write_text(path, text, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_path = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temp_path, mode)
        os.replace(temp_path, path)
    except Exception:
        try:
            os.unlink(temp_path)
        except FileNotFoundError:
            pass
        raise


def main():
    pem_path = pathlib.Path(sys.argv[1])
    server = sys.argv[2]
    config_path = pathlib.Path(sys.argv[3])
    key_file = pathlib.Path(sys.argv[4])
    dry_run = sys.argv[5] == "1"
    clean = sys.argv[6] == "1"

    if any(ch in server for ch in '\r\n"\\'):
        fail("server must not contain newlines, backslashes, or double quotes")
    if not config_path.is_file():
        fail(f"config file not found: {config_path}")

    sk = None
    if clean:
        target_server = ORIGINAL_SERVER
        target_pk = ORIGINAL_PUB_KEY
    else:
        txt = subprocess.check_output(
            ["openssl", "pkey", "-in", str(pem_path), "-text", "-noout"],
            text=True,
        )

        sk_raw = bytes_from_labels(txt, ["priv", "private", "private key"])
        pk_raw = bytes_from_labels(txt, ["pub", "public", "public key"])
        if len(pk_raw) != 32:
            fail(f"unexpected public key length: {len(pk_raw)}")
        if len(sk_raw) == 32:
            secret_bytes = sk_raw + pk_raw
        elif len(sk_raw) == 64:
            if sk_raw[32:] != pk_raw:
                fail("private key tail does not match public key")
            secret_bytes = sk_raw
        else:
            fail(f"unexpected private key length: {len(sk_raw)}")

        sk = base64.b64encode(secret_bytes).decode("ascii")
        target_pk = base64.b64encode(pk_raw).decode("ascii")
        target_server = server

        if len(base64.b64decode(sk, validate=True)) != 64:
            fail("generated secret key payload is not 64 bytes")
        if len(base64.b64decode(target_pk, validate=True)) != 32:
            fail("generated public key payload is not 32 bytes")

    config_text = config_path.read_text(encoding="utf-8")
    updated_text = render_replacement(
        config_text,
        r'^pub const RENDEZVOUS_SERVERS: &\[&str\] = &\[[^\n]*\];$',
        f'pub const RENDEZVOUS_SERVERS: &[&str] = &["{target_server}"];',
        "RENDEZVOUS_SERVERS",
    )
    updated_text = render_replacement(
        updated_text,
        r'^pub const RS_PUB_KEY: &str = "[^"\n]*";$',
        f'pub const RS_PUB_KEY: &str = "{target_pk}";',
        "RS_PUB_KEY",
    )

    if dry_run:
        if clean:
            print(f"dry-run: would restore {config_path}", file=sys.stderr)
        else:
            print(f"dry-run: would update {config_path}", file=sys.stderr)
            print(f"dry-run: would write {key_file}", file=sys.stderr)
    else:
        cfg_mode = stat.S_IMODE(config_path.stat().st_mode) or 0o644
        atomic_write_text(config_path, updated_text, cfg_mode)
        if clean:
            print(f"restored {config_path}", file=sys.stderr)
        else:
            key_text = f"{sk}\n{target_pk}\n"
            atomic_write_text(key_file, key_text, 0o600)
            print(f"updated {config_path}", file=sys.stderr)
            print(f"wrote {key_file}", file=sys.stderr)

    if not clean:
        print(sk)
        print(target_pk)


if __name__ == "__main__":
    main()
PY
