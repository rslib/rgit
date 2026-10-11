#!/usr/bin/env bash
# Does an agent with the rgit Claude plugin (skill, session hook, mod tools)
# use rgit instead of raw git? Runs each case with the plugin and with no
# plugin, on the model given (default haiku), and writes the report to
# evals/agent/results/.
#
#   evals/agent/run.sh [model] [extra claude plugin eval flags]
#
# Uses target/debug/rgit (build it first) unless RGIT names another binary.
# Each run is a real model call: about $0.10 per full run on haiku, $1.40 on sonnet.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
rgit=${RGIT:-$root/target/debug/rgit}
model=${1:-haiku}
shift || true

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
HOME=$tmp/home XDG_DATA_HOME=$tmp/home/.local/share "$rgit" agent install claude >/dev/null
plugin=$tmp/home/.local/share/rgit/claude-plugin/plugins/rgit
cp -R "$here/cases" "$plugin/evals"
for case in "$plugin"/evals/*/; do
  cp "$here/setup.sh" "$case/setup.sh"
done

out=$here/results/$model-$(date -u +%Y-%m-%dT%H-%M-%SZ)
mkdir -p "$out"
prefix=()
command -v taskpolicy >/dev/null && prefix=(taskpolicy -b)
# Only the plugin under test: no memory, bundled skills or CLAUDE.md files.
env CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 CLAUDE_CODE_DISABLE_ORG_MEMORY=1 \
  CLAUDE_CODE_DISABLE_BUNDLED_SKILLS=1 CLAUDE_CODE_DISABLE_CLAUDE_MDS=1 \
  CLAUDE_CODE_DISABLE_WORKFLOWS=1 \
  "${prefix[@]}" claude plugin eval "$plugin" --model "$model" --no-publish \
  --trust-plugin --threshold 0 --runs 3 --scaffold -j 3 \
  --allow-tools Bash Write Edit 'mcp__rgit__*' --max-cost-usd 5 \
  --json "$out/result.json" "$@"
cp -R "$plugin/evals/results/." "$out/" 2>/dev/null || true
echo "results: $out"
