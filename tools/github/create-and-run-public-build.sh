#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
ENV_FILE="${1:-${SCRIPT_DIR}/public-build.env}"
API_VERSION="2022-11-28"
WORKFLOW_FILE="public-free-build.yml"

usage() {
  cat <<'EOF'
Usage: tools/github/create-and-run-public-build.sh [ENV_FILE]

The env file defaults to tools/github/public-build.env.
Required variables:
  GITHUB_OWNER
  GITHUB_REPO
  GITHUB_TOKEN

Recommended token type:
  classic PAT with repo + workflow scopes
EOF
}

if [[ "${ENV_FILE}" == "--help" || "${ENV_FILE}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

: "${GITHUB_OWNER:?GITHUB_OWNER is required}"
: "${GITHUB_REPO:?GITHUB_REPO is required}"
: "${GITHUB_TOKEN:?GITHUB_TOKEN is required}"

GITHUB_OWNER_TYPE="${GITHUB_OWNER_TYPE:-user}"
GITHUB_BRANCH="${GITHUB_BRANCH:-main}"
GIT_AUTHOR_NAME="${GIT_AUTHOR_NAME:-${GITHUB_OWNER}}"
GIT_AUTHOR_EMAIL="${GIT_AUTHOR_EMAIL:-${GITHUB_OWNER}@users.noreply.github.com}"
FORCE_PUSH="${FORCE_PUSH:-1}"
USE_LOCAL_BRIDGE="${USE_LOCAL_BRIDGE:-1}"
WINDOWS_AMD64="${WINDOWS_AMD64:-1}"
MACOS_AMD64="${MACOS_AMD64:-1}"
MACOS_ARM64="${MACOS_ARM64:-1}"
LINUX_AMD64="${LINUX_AMD64:-1}"
LINUX_ARM64="${LINUX_ARM64:-1}"
ANDROID_ARM64="${ANDROID_ARM64:-1}"
IOS_UNSIGNED="${IOS_UNSIGNED:-1}"
REPO_DESCRIPTION="${REPO_DESCRIPTION:-Temporary public build repo for multi-platform RustDesk builds}"

if [[ -n "${GITHUB_PASSWORD:-}" ]]; then
  printf '[public-build][warn] GITHUB_PASSWORD is ignored. GitHub passwords are not supported for Git/API operations.\n' >&2
fi

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    printf '[public-build][error] Missing required command: %s\n' "$1" >&2
    exit 1
  }
}

print_pat_guidance() {
  cat >&2 <<'EOF'
[public-build][hint] The current token can authenticate, but it cannot create or administrate repositories.
[public-build][hint] Fastest fix: use a classic PAT with `repo` and `workflow`.
[public-build][hint] If you must use a fine-grained PAT:
[public-build][hint]   1. Resource owner must be your personal account or target org.
[public-build][hint]   2. Repository access should be `All repositories` when creating a new repo.
[public-build][hint]   3. Grant at least:
[public-build][hint]      - Administration: Read and write
[public-build][hint]      - Actions: Read and write
[public-build][hint]      - Contents: Read and write
EOF
}

bool_string() {
  case "${1:-0}" in
    1|true|TRUE|yes|YES|on|ON) printf 'true\n' ;;
    *) printf 'false\n' ;;
  esac
}

api_request() {
  local method="$1"
  local url="$2"
  local output_file="$3"
  local data="${4:-}"
  local status
  if [[ -n "${data}" ]]; then
    status="$(
      curl -sS \
        -X "${method}" \
        -H "Accept: application/vnd.github+json" \
        -H "Authorization: Bearer ${GITHUB_TOKEN}" \
        -H "X-GitHub-Api-Version: ${API_VERSION}" \
        -o "${output_file}" \
        -w '%{http_code}' \
        "${url}" \
        --data "${data}"
    )"
  else
    status="$(
      curl -sS \
        -X "${method}" \
        -H "Accept: application/vnd.github+json" \
        -H "Authorization: Bearer ${GITHUB_TOKEN}" \
        -H "X-GitHub-Api-Version: ${API_VERSION}" \
        -o "${output_file}" \
        -w '%{http_code}' \
        "${url}"
    )"
  fi
  printf '%s\n' "${status}"
}

require_cmd bash
require_cmd curl
require_cmd git
require_cmd jq
require_cmd mktemp

workdir="$(mktemp -d)"
cleanup() {
  rm -rf "${workdir}"
}
trap cleanup EXIT

export_dir="${workdir}/export"
export_args=(--dest "${export_dir}")
if [[ "$(bool_string "${USE_LOCAL_BRIDGE}")" == "true" ]]; then
  export_args+=(--with-local-bridge)
else
  export_args+=(--skip-local-bridge)
fi

bash "${SCRIPT_DIR}/export-public-source.sh" "${export_args[@]}"

pushd "${export_dir}" >/dev/null
git init -b "${GITHUB_BRANCH}"
git config user.name "${GIT_AUTHOR_NAME}"
git config user.email "${GIT_AUTHOR_EMAIL}"
git add -f -A
if [[ "$(bool_string "${USE_LOCAL_BRIDGE}")" == "true" ]]; then
  git add -f \
    src/bridge_generated.rs \
    src/bridge_generated.io.rs \
    flutter/lib/generated_bridge.dart \
    flutter/lib/generated_bridge.freezed.dart \
    flutter/macos/Runner/bridge_generated.h \
    flutter/ios/Runner/bridge_generated.h
fi
git commit -m "Public build snapshot $(date -u +%Y-%m-%dT%H:%M:%SZ)"
popd >/dev/null

repo_api="https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}"
repo_meta="${workdir}/repo.json"
repo_status="$(api_request GET "${repo_api}" "${repo_meta}")"

if [[ "${repo_status}" == "404" ]]; then
  if [[ "${GITHUB_OWNER_TYPE}" == "org" ]]; then
    create_url="https://api.github.com/orgs/${GITHUB_OWNER}/repos"
  else
    create_url="https://api.github.com/user/repos"
  fi
  create_payload="$(
    jq -n \
      --arg name "${GITHUB_REPO}" \
      --arg description "${REPO_DESCRIPTION}" \
      '{
        name: $name,
        description: $description,
        private: false,
        has_issues: false,
        has_projects: false,
        has_wiki: false,
        delete_branch_on_merge: true,
        auto_init: false
      }'
  )"
  create_status="$(api_request POST "${create_url}" "${repo_meta}" "${create_payload}")"
  [[ "${create_status}" == "201" ]] || {
    printf '[public-build][error] Failed to create repository: %s\n' "${create_status}" >&2
    cat "${repo_meta}" >&2
    if [[ "${create_status}" == "403" ]]; then
      print_pat_guidance
    fi
    exit 1
  }
elif [[ "${repo_status}" != "200" ]]; then
  printf '[public-build][error] Failed to inspect repository: %s\n' "${repo_status}" >&2
  cat "${repo_meta}" >&2
  exit 1
fi

patch_payload="$(
  jq -n \
    --arg description "${REPO_DESCRIPTION}" \
    '{
      description: $description,
      private: false,
      has_issues: false,
      has_projects: false,
      has_wiki: false,
      delete_branch_on_merge: true
    }'
)"
patch_status="$(api_request PATCH "${repo_api}" "${repo_meta}" "${patch_payload}")"
[[ "${patch_status}" == "200" ]] || {
  printf '[public-build][error] Failed to update repository metadata: %s\n' "${patch_status}" >&2
  cat "${repo_meta}" >&2
  if [[ "${patch_status}" == "403" ]]; then
    print_pat_guidance
  fi
  exit 1
}

pushd "${export_dir}" >/dev/null
git remote add origin "https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}.git"
push_url="https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_OWNER}/${GITHUB_REPO}.git"
if [[ "${FORCE_PUSH}" == "1" ]]; then
  git push --force "${push_url}" "HEAD:${GITHUB_BRANCH}"
else
  git push "${push_url}" "HEAD:${GITHUB_BRANCH}"
fi
popd >/dev/null

post_push_payload="$(
  jq -n \
    --arg default_branch "${GITHUB_BRANCH}" \
    '{
      default_branch: $default_branch
    }'
)"
post_push_status="$(api_request PATCH "${repo_api}" "${repo_meta}" "${post_push_payload}")"
[[ "${post_push_status}" == "200" ]] || {
  printf '[public-build][error] Failed to set default branch after push: %s\n' "${post_push_status}" >&2
  cat "${repo_meta}" >&2
  if [[ "${post_push_status}" == "403" ]]; then
    print_pat_guidance
  fi
  exit 1
}

workflow_meta="${workdir}/workflow.json"
workflow_url="${repo_api}/actions/workflows/${WORKFLOW_FILE}"
workflow_ready=0
for _attempt in $(seq 1 12); do
  workflow_status="$(api_request GET "${workflow_url}" "${workflow_meta}")"
  if [[ "${workflow_status}" == "200" ]]; then
    workflow_ready=1
    break
  fi
  sleep 5
done

[[ "${workflow_ready}" == "1" ]] || {
  printf '[public-build][error] Workflow %s was not indexed in time.\n' "${WORKFLOW_FILE}" >&2
  cat "${workflow_meta}" >&2
  exit 1
}

dispatch_meta="${workdir}/dispatch.json"
dispatch_payload="$(
  jq -n \
    --arg ref "${GITHUB_BRANCH}" \
    --arg use_local_bridge "$(bool_string "${USE_LOCAL_BRIDGE}")" \
    --arg windows_amd64 "$(bool_string "${WINDOWS_AMD64}")" \
    --arg macos_amd64 "$(bool_string "${MACOS_AMD64}")" \
    --arg macos_arm64 "$(bool_string "${MACOS_ARM64}")" \
    --arg linux_amd64 "$(bool_string "${LINUX_AMD64}")" \
    --arg linux_arm64 "$(bool_string "${LINUX_ARM64}")" \
    --arg android_arm64 "$(bool_string "${ANDROID_ARM64}")" \
    --arg ios_unsigned "$(bool_string "${IOS_UNSIGNED}")" \
    '{
      ref: $ref,
      inputs: {
        use_local_bridge: $use_local_bridge,
        windows_amd64: $windows_amd64,
        macos_amd64: $macos_amd64,
        macos_arm64: $macos_arm64,
        linux_amd64: $linux_amd64,
        linux_arm64: $linux_arm64,
        android_arm64: $android_arm64,
        ios_unsigned: $ios_unsigned
      },
      return_run_details: true
    }'
)"
dispatch_status="$(api_request POST "${workflow_url}/dispatches" "${dispatch_meta}" "${dispatch_payload}")"
if [[ "${dispatch_status}" != "200" && "${dispatch_status}" != "204" ]]; then
  printf '[public-build][error] Failed to dispatch workflow: %s\n' "${dispatch_status}" >&2
  cat "${dispatch_meta}" >&2
  if [[ "${dispatch_status}" == "403" ]]; then
    print_pat_guidance
  fi
  exit 1
fi

run_url="$(jq -r '.html_url // empty' "${dispatch_meta}" 2>/dev/null || true)"
if [[ -z "${run_url}" ]]; then
  sleep 5
  runs_meta="${workdir}/runs.json"
  runs_status="$(api_request GET "${repo_api}/actions/runs?event=workflow_dispatch&branch=${GITHUB_BRANCH}&per_page=1" "${runs_meta}")"
  if [[ "${runs_status}" == "200" ]]; then
    run_url="$(jq -r '.workflow_runs[0].html_url // empty' "${runs_meta}")"
  fi
fi

printf '[public-build] Repository: https://github.com/%s/%s\n' "${GITHUB_OWNER}" "${GITHUB_REPO}"
if [[ -n "${run_url}" ]]; then
  printf '[public-build] Workflow run: %s\n' "${run_url}"
else
  printf '[public-build] Workflow dispatched. Open Actions tab manually if the run URL is not shown yet.\n'
fi
