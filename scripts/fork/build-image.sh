#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Local, low-impact build of the fork's Temps image (linux/amd64).
#
# This is the default way to produce a fork image. It mirrors the steps of
# .github/workflows/fork-image.yml (the manual GitHub fallback), which in turn
# mirrors release.yml:
#
#   1. web dependencies         bun install --frozen-lockfile        (web/)
#   2. captcha WASM module      bun run build                        (crates/temps-captcha-wasm/)
#   3. web dist                 bun run build with RSBUILD_OUTPUT_PATH=crates/temps-cli/dist
#   4. release binary           cargo build --release --bin temps
#                               (SKIP_WEB_BUILD=1, CARGO_INCREMENTAL=0, TEMPS_VERSION=<ref>-<sha12>)
#                               inside an ubuntu-22.04 builder container (see below)
#   5. strip + copy             artifacts/temps
#   6. image                    docker build -f Dockerfile.release
#
# Gentle on a shared workstation: every cargo invocation goes through
# cargo-gentle (job cap, nice, and mold when installed), including the ones
# wasm-pack makes on its own, and the bun steps run in a transient systemd user
# scope with a low CPU weight and a memory ceiling. Run one build at a time.
#
# Why a builder container. The release binary has to run on the runtime image
# (debian:bookworm-slim, glibc 2.36). CI compiles on ubuntu-22.04 (glibc 2.35) for
# exactly that reason; a binary linked on a newer host (Debian 13 has glibc 2.41)
# dies at start-up with "GLIBC_2.38 not found". BUILD_MODE=container (default)
# therefore runs step 4 in scripts/fork/Dockerfile.builder, an ubuntu-22.04 image
# with CI's apt packages and a rustup toolchain. There cargo-gentle cannot open
# a systemd scope, so the limits come from docker run flags instead:
# --cpu-shares (a low CPU weight, below default containers), --memory with a
# small swap allowance (a hard cap, sized above the observed peak), and
# GENTLE_JOBS for the job count. BUILD_MODE=host compiles on the host through
# cargo-gentle's systemd scope and refuses to run when the host glibc is newer
# than the runtime image's.
#
# Notes on the WASM step. crates/temps-proxy embeds the *committed*
# crates/temps-captcha-wasm/pkg files with include_str!/include_bytes!, and CI
# compiles the binary from a fresh checkout, so the pkg that CI's web-assets job
# rebuilds never reaches the binary. To produce the same bytes as CI, this script
# rebuilds the module (so a broken WASM crate fails the build exactly as it does
# in CI), reports whether the fresh output differs from the committed files, and
# then restores the committed pkg files with `git checkout` before compiling the
# binary. Set SKIP_WASM=1 to skip the rebuild entirely.
#
# Usage (from anywhere):
#   scripts/fork/build-image.sh
#
# Environment overrides (all optional):
#   BUILD_MODE          container (default) or host
#   CARGO_TARGET_DIR    default: $HOME/.cache/temps-cargo-target/<BUILD_MODE>
#                       (container and host builds link different glibc versions,
#                       so they must never share a target directory)
#   CARGO_GENTLE        default: $HOME/.claude/skills/gentle-rust-builds/scripts/cargo-gentle
#   BUILDER_IMAGE       default: temps-fork-builder:ubuntu-22.04 (built on first use)
#   REBUILD_BUILDER=1   rebuild the builder image (pulls ubuntu:22.04, refreshes rustup stable)
#   BUILD_CPU_SHARES    default: 256   (docker --cpu-shares; about cpu.weight 10, container mode)
#   BUILD_MEMORY        default: 16g   (docker --memory, container mode)
#   BUILD_MEMORY_SWAP   default: 20g   (docker --memory-swap = memory + swap, container mode)
#   BUILD_JOBS          default: GENTLE_JOBS or 10 (cargo jobs, container mode)
#   IMAGE_REPO          default: temps-fork
#   SOURCE_URL          default: https://github.com/thecotne/temps  (OCI source label)
#   MIN_FREE_GB         default: 60   (abort if `/` or the target dir filesystem has less)
#   MAX_MEM_PSI_FULL    default: 5    (abort if /proc/pressure/memory full avg60 is above this)
#   ALLOW_DIRTY=1       build with uncommitted tracked changes; tags get a -dirty
#                       suffix and the branch tag is skipped
#   SKIP_WASM=1         skip the captcha WASM rebuild
#   GENTLE_CPU_WEIGHT, GENTLE_MEMORY_HIGH, GENTLE_SWAP_MAX, GENTLE_JOBS,
#   GENTLE_NO_MOLD      passed through to cargo-gentle and the bun steps (see the
#                       gentle-rust-builds skill); host mode uses all of them
#
# Nothing is pushed anywhere and no existing image is retagged. Previous web
# dist output is moved aside (never deleted) into $HOME/.cache/temps-build/old-dist
# so a stale hashed asset cannot end up embedded in the binary.

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

BUILD_MODE="${BUILD_MODE:-container}"
case "${BUILD_MODE}" in
  container | host) ;;
  *) printf 'ERROR: BUILD_MODE must be "container" or "host", got "%s"\n' "${BUILD_MODE}" >&2; exit 1 ;;
esac
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${HOME}/.cache/temps-cargo-target/${BUILD_MODE}}"
CARGO_GENTLE="${CARGO_GENTLE:-${HOME}/.claude/skills/gentle-rust-builds/scripts/cargo-gentle}"
IMAGE_REPO="${IMAGE_REPO:-temps-fork}"
SOURCE_URL="${SOURCE_URL:-https://github.com/thecotne/temps}"
MIN_FREE_GB="${MIN_FREE_GB:-60}"
MAX_MEM_PSI_FULL="${MAX_MEM_PSI_FULL:-5}"
ALLOW_DIRTY="${ALLOW_DIRTY:-0}"
SKIP_WASM="${SKIP_WASM:-0}"
BUILDER_IMAGE="${BUILDER_IMAGE:-temps-fork-builder:ubuntu-22.04}"
REBUILD_BUILDER="${REBUILD_BUILDER:-0}"
BUILD_CPU_SHARES="${BUILD_CPU_SHARES:-256}"
BUILD_MEMORY="${BUILD_MEMORY:-16g}"
BUILD_MEMORY_SWAP="${BUILD_MEMORY_SWAP:-20g}"
BUILD_JOBS="${BUILD_JOBS:-${GENTLE_JOBS:-10}}"
# glibc of the runtime image in Dockerfile.release (debian:bookworm-slim).
RUNTIME_GLIBC="2.36"
BUILDER_CONTAINER_NAME="temps-fork-build"

BUILD_HOME="${HOME}/.cache/temps-build"
SHIM_DIR="${BUILD_HOME}/cargo-shim"
OLD_DIST_DIR="${BUILD_HOME}/old-dist"
CONTAINER_CARGO_HOME="${BUILD_HOME}/cargo-home"
BUILDER_DOCKERFILE="${SCRIPT_DIR}/Dockerfile.builder"

WEB_DIR="${REPO_ROOT}/web"
WASM_DIR="${REPO_ROOT}/crates/temps-captcha-wasm"
WASM_PKG_REL="crates/temps-captcha-wasm/pkg"
DIST_DIR="${REPO_ROOT}/crates/temps-cli/dist"
ARTIFACTS_DIR="${REPO_ROOT}/artifacts"
ARTIFACT_BIN="${ARTIFACTS_DIR}/temps"
CARGO_OUTPUT_BIN="${CARGO_TARGET_DIR}/release/temps"

export CARGO_TARGET_DIR
export CARGO_NET_GIT_FETCH_WITH_CLI=true
export RUST_BACKTRACE=1

# Version comparison: succeeds when $1 <= $2 (dotted numbers).
version_le() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$1" ]; }

START_SECONDS=${SECONDS}
CURRENT_STEP="startup"

log() { printf '[%s] %s\n' "$(date '+%H:%M:%S')" "$*"; }
die() { printf '[%s] ERROR: %s\n' "$(date '+%H:%M:%S')" "$*" >&2; exit 1; }

on_error() {
  local code=$?
  printf '[%s] FAILED during step "%s" (exit %s) after %ss\n' \
    "$(date '+%H:%M:%S')" "${CURRENT_STEP}" "${code}" "$((SECONDS - START_SECONDS))" >&2
  exit "${code}"
}
trap on_error ERR

step() {
  CURRENT_STEP="$1"
  printf '\n[%s] ===== %s (elapsed %ss) =====\n' \
    "$(date '+%H:%M:%S')" "${CURRENT_STEP}" "$((SECONDS - START_SECONDS))"
  snapshot
}

# One-line view of memory, swap and pressure so a log shows how the machine
# behaved while the build ran.
snapshot() {
  local mem_avail_kb swap_total_kb swap_free_kb mem_some mem_full cpu_some
  mem_avail_kb="$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)"
  swap_total_kb="$(awk '/^SwapTotal:/ {print $2}' /proc/meminfo)"
  swap_free_kb="$(awk '/^SwapFree:/ {print $2}' /proc/meminfo)"
  mem_some="$(psi_value /proc/pressure/memory some avg10)"
  mem_full="$(psi_value /proc/pressure/memory full avg10)"
  cpu_some="$(psi_value /proc/pressure/cpu some avg10)"
  printf '    load="%s" mem_available=%sMiB swap_used=%sMiB psi_mem_some10=%s psi_mem_full10=%s psi_cpu_some10=%s\n' \
    "$(cut -d' ' -f1-3 /proc/loadavg)" \
    "$((mem_avail_kb / 1024))" "$(((swap_total_kb - swap_free_kb) / 1024))" \
    "${mem_some}" "${mem_full}" "${cpu_some}"
}

# psi_value <file> <some|full> <avg10|avg60|avg300>
psi_value() {
  awk -v kind="$2" -v key="$3" '
    $1 == kind { for (i = 2; i <= NF; i++) { split($i, kv, "="); if (kv[1] == key) print kv[2] } }
  ' "$1"
}

# Free space, in whole GiB, of the filesystem holding the given path.
free_gb() {
  df --output=avail -BG "$1" | tail -n 1 | tr -dc '0-9'
}

# Run a command with the same limits cargo-gentle applies (transient systemd
# user scope: low CPU weight, memory ceiling, capped swap), or under nice when
# no user systemd is reachable. Used for the bun steps; cargo has its own wrapper.
run_gentle() {
  if [ "$(uname -s)" = Linux ] && command -v systemd-run >/dev/null 2>&1 \
    && systemctl --user show-environment >/dev/null 2>&1; then
    local memory_high="${GENTLE_MEMORY_HIGH:-}"
    if [ -z "${memory_high}" ]; then
      memory_high="$(($(awk '/^MemTotal:/ {print $2}' /proc/meminfo) / 4 / 1024))M"
    fi
    systemd-run --user --scope --quiet --collect \
      -p CPUWeight="${GENTLE_CPU_WEIGHT:-20}" \
      -p MemoryHigh="${memory_high}" \
      -p MemorySwapMax="${GENTLE_SWAP_MAX:-4G}" \
      -- "$@"
  else
    nice -n 10 "$@"
  fi
}

# Same sanitizer as fork-image.yml: Docker tag grammar
# [A-Za-z0-9_][A-Za-z0-9_.-]{0,127}.
sanitize_tag() {
  local value
  value="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9_.-]+/-/g; s/^[.-]+//; s/-+$//')"
  printf '%s' "${value:0:128}"
}

# ---------------------------------------------------------------------------
step "Preflight"

cd "${REPO_ROOT}"
log "repository root: ${REPO_ROOT}"
log "build mode: ${BUILD_MODE}"
log "cargo target dir: ${CARGO_TARGET_DIR}"
log "cargo-gentle: ${CARGO_GENTLE}"

[ -x "${CARGO_GENTLE}" ] || die "cargo-gentle not found or not executable at ${CARGO_GENTLE}"
for tool in git bun docker strip file python3; do
  command -v "${tool}" >/dev/null 2>&1 || die "required tool not found on PATH: ${tool}"
done
if [ "${SKIP_WASM}" != 1 ]; then
  for tool in wasm-pack rustup; do
    command -v "${tool}" >/dev/null 2>&1 || die "required tool not found on PATH: ${tool}"
  done
fi
if [ "${BUILD_MODE}" = host ]; then
  for tool in cargo protoc cmake pkg-config; do
    command -v "${tool}" >/dev/null 2>&1 || die "required tool not found on PATH for BUILD_MODE=host: ${tool}"
  done
  host_glibc="$(ldd --version | head -n 1 | grep -oE '[0-9]+\.[0-9]+$')"
  log "host glibc ${host_glibc}, runtime image glibc ${RUNTIME_GLIBC}"
  version_le "${host_glibc}" "${RUNTIME_GLIBC}" \
    || die "host glibc ${host_glibc} is newer than the runtime image's ${RUNTIME_GLIBC}; the binary would not start in the image. Use BUILD_MODE=container."
fi
[ "${BUILD_MODE}" = container ] && [ ! -f "${BUILDER_DOCKERFILE}" ] \
  && die "missing ${BUILDER_DOCKERFILE}"
if [ "${SKIP_WASM}" != 1 ]; then
  rustup target list --installed | grep -qx 'wasm32-unknown-unknown' \
    || die "missing rust target: run 'rustup target add wasm32-unknown-unknown'"
fi
docker info >/dev/null 2>&1 || die "cannot talk to the Docker daemon"

mkdir -p "${CARGO_TARGET_DIR}"
for guard_path in / "${CARGO_TARGET_DIR}"; do
  free="$(free_gb "${guard_path}")"
  log "free space on the filesystem of ${guard_path}: ${free}G (minimum ${MIN_FREE_GB}G)"
  [ "${free}" -ge "${MIN_FREE_GB}" ] \
    || die "only ${free}G free on the filesystem of ${guard_path}; need at least ${MIN_FREE_GB}G (release artifacts are large)"
done

mem_full_avg60="$(psi_value /proc/pressure/memory full avg60)"
log "memory pressure (full avg60): ${mem_full_avg60}% (limit ${MAX_MEM_PSI_FULL}%)"
awk -v have="${mem_full_avg60}" -v max="${MAX_MEM_PSI_FULL}" 'BEGIN { exit !(have + 0 > max + 0) }' \
  && die "memory pressure is already high (full avg60 ${mem_full_avg60}%); wait, or lower GENTLE_JOBS/GENTLE_MEMORY_HIGH"

# ---------------------------------------------------------------------------
step "Record git revision"

SHA_FULL="$(git rev-parse HEAD)"
SHA12="${SHA_FULL:0:12}"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
REF_TAG="$(sanitize_tag "${BRANCH}")"
[ -n "${REF_TAG}" ] || die "branch name '${BRANCH}' sanitizes to an empty Docker tag"

DIRTY_SUFFIX=""
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  if [ "${ALLOW_DIRTY}" = 1 ]; then
    DIRTY_SUFFIX="-dirty"
    log "WARNING: uncommitted tracked changes present; tags get a -dirty suffix (ALLOW_DIRTY=1)"
  else
    git status --short --untracked-files=no >&2
    die "working tree has uncommitted tracked changes; commit or stash them, or set ALLOW_DIRTY=1"
  fi
fi

VERSION="${REF_TAG}-${SHA12}${DIRTY_SUFFIX}"
SHA_TAG="${IMAGE_REPO}:sha-${SHA12}${DIRTY_SUFFIX}"
TAGS=("${SHA_TAG}")
if [ "${BRANCH}" = production ] && [ -z "${DIRTY_SUFFIX}" ]; then
  TAGS+=("${IMAGE_REPO}:production")
fi
log "revision: ${SHA_FULL}"
log "sha12:    ${SHA12}"
log "branch:   ${BRANCH}"
log "version:  ${VERSION}"
log "tags:     ${TAGS[*]}"

# ---------------------------------------------------------------------------
step "Install web dependencies (bun install --frozen-lockfile)"

(cd "${WEB_DIR}" && run_gentle bun install --frozen-lockfile)

# ---------------------------------------------------------------------------
if [ "${SKIP_WASM}" = 1 ]; then
  step "Captcha WASM module (skipped: SKIP_WASM=1)"
else
  step "Build captcha WASM module (wasm-pack via bun run build)"

  if [ -n "$(git status --porcelain -- "${WASM_PKG_REL}")" ]; then
    die "${WASM_PKG_REL} has local changes; this script would overwrite them. Commit or stash them first."
  fi

  # wasm-pack runs `cargo` from PATH. Route those calls through cargo-gentle so
  # even this small compile respects the limits. The shim is a plain file that
  # is rewritten on every run and never deleted. mold is disabled for it because
  # the wasm32 target is linked by wasm-ld.
  mkdir -p "${SHIM_DIR}"
  cat > "${SHIM_DIR}/cargo" <<'SHIM'
#!/usr/bin/env bash
# Generated by scripts/fork/build-image.sh: run cargo through cargo-gentle.
set -Eeuo pipefail
PATH="${TEMPS_BUILD_ORIG_PATH:?TEMPS_BUILD_ORIG_PATH must be set}"
export GENTLE_NO_MOLD=1
exec "${TEMPS_BUILD_CARGO_GENTLE:?TEMPS_BUILD_CARGO_GENTLE must be set}" "$@"
SHIM
  chmod 0755 "${SHIM_DIR}/cargo"
  log "cargo shim written to ${SHIM_DIR}/cargo"

  (
    cd "${WASM_DIR}"
    TEMPS_BUILD_ORIG_PATH="${PATH}" \
    TEMPS_BUILD_CARGO_GENTLE="${CARGO_GENTLE}" \
    PATH="${SHIM_DIR}:${PATH}" \
      bun run build
  )

  if [ -n "$(git status --porcelain -- "${WASM_PKG_REL}")" ]; then
    log "NOTE: the fresh WASM output differs from the committed ${WASM_PKG_REL} files:"
    git status --short -- "${WASM_PKG_REL}"
    git diff --stat -- "${WASM_PKG_REL}" || true
    log "restoring the committed files so the binary embeds the same bytes as a CI build"
    git checkout -- "${WASM_PKG_REL}"
  else
    log "fresh WASM output is byte-identical to the committed ${WASM_PKG_REL} files"
  fi
fi

# ---------------------------------------------------------------------------
step "Build web dist (bun run build)"

# Rsbuild does not clean an output directory that lives outside the web project,
# so a previous build's hashed assets would otherwise be embedded next to the new
# ones. Move an existing dist aside (never delete) unless it is only the
# placeholder that build.rs creates.
if [ -d "${DIST_DIR}" ]; then
  entries="$(find "${DIST_DIR}" -mindepth 1 | wc -l)"
  if [ "${entries}" -gt 1 ]; then
    mkdir -p "${OLD_DIST_DIR}"
    aside="${OLD_DIST_DIR}/dist-$(date '+%Y%m%d-%H%M%S')-$$"
    log "moving previous dist (${entries} entries) aside to ${aside}"
    mv "${DIST_DIR}" "${aside}"
  else
    log "existing dist is only the placeholder; rsbuild will overwrite it"
  fi
fi

(
  cd "${WEB_DIR}"
  run_gentle env \
    RSBUILD_OUTPUT_PATH="${DIST_DIR}" \
    TEMPS_VERSION="${VERSION}" \
    bun run build
)
[ -f "${DIST_DIR}/index.html" ] || die "web build finished but ${DIST_DIR}/index.html is missing"
log "web dist ready: $(find "${DIST_DIR}" -type f | wc -l) files in ${DIST_DIR}"

# ---------------------------------------------------------------------------
if [ "${BUILD_MODE}" = container ]; then
  step "Prepare builder image (${BUILDER_IMAGE})"

  if [ "${REBUILD_BUILDER}" = 1 ] || ! docker image inspect "${BUILDER_IMAGE}" >/dev/null 2>&1; then
    builder_args=(--platform linux/amd64 --progress plain --tag "${BUILDER_IMAGE}")
    if [ "${REBUILD_BUILDER}" = 1 ]; then
      builder_args+=(--pull --no-cache)
    fi
    # No build context: the Dockerfile is read from stdin.
    docker build "${builder_args[@]}" - < "${BUILDER_DOCKERFILE}"
  else
    log "builder image already present (REBUILD_BUILDER=1 to refresh it)"
  fi

  step "Build release binary in the builder container (cargo-gentle build --release --bin temps)"

  # Same flags and environment as the CI "Build release binary" step. The
  # fork-image workflow sets no TEMPS_RELEASE_IMAGE_MANIFEST, so neither do we.
  #
  # The repository, git directory, target dir and cargo home are mounted at their
  # host paths so build.rs sees the same layout (git describe, embedded paths).
  # The git directory is read-only. Files are written as the calling user.
  git_common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)"
  mkdir -p "${CONTAINER_CARGO_HOME}"
  log "container limits: cpu-shares=${BUILD_CPU_SHARES} memory=${BUILD_MEMORY} memory-swap=${BUILD_MEMORY_SWAP} jobs=${BUILD_JOBS}"
  docker run --rm \
    --name "${BUILDER_CONTAINER_NAME}" \
    --platform linux/amd64 \
    --user "$(id -u):$(id -g)" \
    --cpu-shares "${BUILD_CPU_SHARES}" \
    --memory "${BUILD_MEMORY}" \
    --memory-swap "${BUILD_MEMORY_SWAP}" \
    --volume "${REPO_ROOT}:${REPO_ROOT}" \
    --volume "${git_common_dir}:${git_common_dir}:ro" \
    --volume "${CARGO_TARGET_DIR}:${CARGO_TARGET_DIR}" \
    --volume "${CONTAINER_CARGO_HOME}:${CONTAINER_CARGO_HOME}" \
    --volume "${CARGO_GENTLE}:/usr/local/bin/cargo-gentle:ro" \
    --workdir "${REPO_ROOT}" \
    --env HOME=/tmp \
    --env CARGO_HOME="${CONTAINER_CARGO_HOME}" \
    --env CARGO_TARGET_DIR="${CARGO_TARGET_DIR}" \
    --env CARGO_NET_GIT_FETCH_WITH_CLI=true \
    --env RUST_BACKTRACE=1 \
    --env SKIP_WEB_BUILD=1 \
    --env CARGO_INCREMENTAL=0 \
    --env TEMPS_VERSION="${VERSION}" \
    --env GENTLE_JOBS="${BUILD_JOBS}" \
    --env GIT_CONFIG_COUNT=1 \
    --env GIT_CONFIG_KEY_0=safe.directory \
    --env GIT_CONFIG_VALUE_0='*' \
    "${BUILDER_IMAGE}" \
    cargo-gentle build --release --bin temps
else
  step "Build release binary on the host (cargo-gentle build --release --bin temps)"

  # Same flags and environment as the CI "Build release binary" step.
  # RUSTFLAGS is left unset so cargo-gentle may add its mold link argument.
  SKIP_WEB_BUILD=1 CARGO_INCREMENTAL=0 TEMPS_VERSION="${VERSION}" \
    "${CARGO_GENTLE}" build --release --bin temps
fi

[ -f "${CARGO_OUTPUT_BIN}" ] || die "cargo finished but ${CARGO_OUTPUT_BIN} is missing"

# ---------------------------------------------------------------------------
step "Copy and strip binary into artifacts/temps"

mkdir -p "${ARTIFACTS_DIR}"
# Keep the directory out of git status without touching the tracked .gitignore.
printf '*\n' > "${ARTIFACTS_DIR}/.gitignore"
# cp -f overwrites the previous artifacts/temps in place; nothing is deleted.
cp -f "${CARGO_OUTPUT_BIN}" "${ARTIFACT_BIN}"
strip "${ARTIFACT_BIN}"
chmod +x "${ARTIFACT_BIN}"
ls -lh "${ARTIFACT_BIN}"
file "${ARTIFACT_BIN}"
"${ARTIFACT_BIN}" --version || log "Note: binary may require runtime dependencies (same fallback as CI)"

# ---------------------------------------------------------------------------
step "Build image (docker build -f Dockerfile.release)"

TAG_ARGS=()
for tag in "${TAGS[@]}"; do
  TAG_ARGS+=(--tag "${tag}")
done

docker build \
  --file "${REPO_ROOT}/Dockerfile.release" \
  --platform linux/amd64 \
  --progress plain \
  --provenance=false \
  --sbom=false \
  "${TAG_ARGS[@]}" \
  --label "org.opencontainers.image.source=${SOURCE_URL}" \
  --label "org.opencontainers.image.revision=${SHA_FULL}" \
  --label "org.opencontainers.image.version=${VERSION}" \
  --label "org.opencontainers.image.title=Temps" \
  --label "org.opencontainers.image.description=Open-source platform for deploying and managing applications" \
  "${REPO_ROOT}"

# ---------------------------------------------------------------------------
step "Result"

IMAGE_ID="$(docker image inspect --format '{{.Id}}' "${SHA_TAG}")"
IMAGE_SIZE="$(docker image inspect --format '{{.Size}}' "${SHA_TAG}")"
log "smoke test: temps --version inside ${SHA_TAG}"
docker run --rm --pull never --network none --memory 256m --entrypoint temps "${SHA_TAG}" --version

printf '\n'
printf 'Image ID:  %s\n' "${IMAGE_ID}"
printf 'Size:      %s bytes\n' "${IMAGE_SIZE}"
printf 'Revision:  %s\n' "${SHA_FULL}"
printf 'Version:   %s\n' "${VERSION}"
printf 'Tags:\n'
for tag in "${TAGS[@]}"; do
  printf '  %s\n' "${tag}"
done
printf 'Binary:    %s\n' "${ARTIFACT_BIN}"
printf 'Total:     %ss\n' "$((SECONDS - START_SECONDS))"
