#!/usr/bin/env bash
# Thin entry point so build.ps1 can reuse the one svn implementation rather than
# carrying a second copy of it. Git for Windows ships bash, and CI already runs
# every leg with `shell: bash` (RL-102).
set -euo pipefail
FIXTURE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=fixtures/svn.sh
source "${FIXTURE_ROOT}/svn.sh"
build_svn_fixture "${1:-${FIXTURE_ROOT}/out}"
