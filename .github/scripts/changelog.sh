#!/usr/bin/env bash
# Release notes from the Conventional Commit subjects between the previous v* tag and TAG.
# Usage: changelog.sh TAG [OWNER/REPO]
set -euo pipefail
tag=$1
repo=${2:-rayandrew/rgit}
prev=$(git tag --list 'v*' --sort=-v:refname | grep -vx "$tag" | head -n1 || true)
range=$tag
[ -n "$prev" ] && range="$prev..$tag"

lines=$(git log --pretty=tformat:'%h|%s' "$range" | while IFS='|' read -r hash msg; do
  [ -z "$msg" ] && continue
  # Version bumps are release plumbing, not changes.
  if printf '%s' "$msg" | grep -qE '^chore(\([^)]*\))?!?: bump version'; then continue; fi
  type=$(printf '%s' "$msg" | sed -E 's/^([a-z]+).*/\1/')
  [ "$type" = tests ] && type="test"
  printf '%s' "$type" | grep -qxE 'feat|fix|perf|refactor|docs|test|build|ci|chore' || type=other
  mark=""
  if printf '%s' "$msg" | grep -qE '^[a-z]+(\([^)]*\))?!:'; then mark="**BREAKING** "; fi
  printf '%s\t- %s%s ([%s](https://github.com/%s/commit/%s))\n' "$type" "$mark" "$msg" "$hash" "$repo" "$hash"
done)

out=""
for pair in "feat:Features" "fix:Bug Fixes" "perf:Performance" "refactor:Refactoring" "docs:Documentation" \
  "test:Tests" "build:Build" "ci:CI" "chore:Chores" "other:Other Changes"; do
  type=${pair%%:*}
  items=$(printf '%s\n' "$lines" | awk -F'\t' -v t="$type" '$1 == t { print $2 }')
  [ -n "$items" ] && out="$out## ${pair#*:}"$'\n'"$items"$'\n\n'
done
[ -z "$out" ] && out="No notable changes in this release."$'\n\n'
[ -n "$prev" ] && out="$out**Full Changelog**: https://github.com/$repo/compare/$prev...$tag"$'\n'
printf '%s' "$out"
