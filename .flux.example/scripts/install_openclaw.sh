#!/usr/bin/env bash
# Install openclaw, link ~/.openclaw to repo config, and start gateway in background.
# Requires: node (>=20), pnpm
# Usage: bash install_openclaw.sh

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_OPENCLAW_DIR="${SCRIPT_DIR}/.openclaw"
OPENCLAW_HOME="${HOME}/.openclaw"
LOGFILE="${HOME}/openclaw.log"

log() {
  printf '[%s] %s\n' "$1" "$2"
}

die() {
  log "error" "$1" >&2
  exit 1
}

require_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || die "Missing required command: ${cmd}"
}

check_node_version() {
  local node_major

  node_major="$(node -p 'Number(process.versions.node.split(".")[0])')"
  if (( node_major < 20 )); then
    die "Node >=20 is required, found $(node --version)"
  fi
}

ensure_prerequisites() {
  require_cmd node
  require_cmd pnpm
  require_cmd pgrep
  check_node_version
}

ensure_openclaw_home_link() {
  local current_target backup_path

  [[ -d "${REPO_OPENCLAW_DIR}" ]] || die "Missing repo config directory: ${REPO_OPENCLAW_DIR}"

  if [[ -L "${OPENCLAW_HOME}" ]]; then
    current_target="$(readlink "${OPENCLAW_HOME}")"
    if [[ "${current_target}" == "${REPO_OPENCLAW_DIR}" ]]; then
      log "skip" "~/.openclaw already linked to repo config"
      return
    fi
  fi

  if [[ -e "${OPENCLAW_HOME}" || -L "${OPENCLAW_HOME}" ]]; then
    backup_path="${OPENCLAW_HOME}.bak.$(date +%s)"
    log "backup" "Moving existing ~/.openclaw to ${backup_path}"
    mv "${OPENCLAW_HOME}" "${backup_path}"
  fi

  log "link" "Linking ~/.openclaw -> ${REPO_OPENCLAW_DIR}"
  ln -s "${REPO_OPENCLAW_DIR}" "${OPENCLAW_HOME}"
}

install_openclaw() {
  local version

  if command -v openclaw >/dev/null 2>&1; then
    version="$(openclaw --version 2>/dev/null | head -1 || true)"
    log "skip" "openclaw already installed${version:+: ${version}}"
    return
  fi

  log "install" "Installing openclaw"
  pnpm add -g openclaw
}

stop_existing_gateway() {
  local pid
  local -a pids=()

  while IFS= read -r pid; do
    [[ -n "${pid}" ]] && pids+=("${pid}")
  done < <(pgrep -f "openclaw.*gateway" 2>/dev/null || true)

  if (( ${#pids[@]} == 0 )); then
    return
  fi

  log "restart" "Stopping existing gateway PIDs: ${pids[*]}"
  kill "${pids[@]}" 2>/dev/null || true
  sleep 2
}

start_gateway() {
  local pid

  stop_existing_gateway

  log "start" "Starting openclaw gateway -> ${LOGFILE}"
  nohup env OPENCLAW_HEADLESS=true openclaw gateway run >> "${LOGFILE}" 2>&1 &
  sleep 3

  pid="$(pgrep -f "openclaw.*gateway" 2>/dev/null | head -1 || true)"
  if [[ -z "${pid}" ]]; then
    log "error" "Gateway failed to start, check ${LOGFILE}"
    tail -20 "${LOGFILE}" 2>/dev/null || true
    exit 1
  fi

  log "done" "openclaw gateway running (PID: ${pid})"
  log "dashboard" "Run 'openclaw dashboard --no-open' to get the Web UI URL"
}

main() {
  ensure_prerequisites
  ensure_openclaw_home_link
  install_openclaw
  start_gateway
}

main "$@"
