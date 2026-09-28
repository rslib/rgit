#!/bin/sh
# A stand-in for Claude Code's `claude plugin` commands, so tests never touch a real Claude Code.
# It logs every call and keeps its state next to itself: `mk` (the marketplace path) and `ver` (the installed version).
D=$(dirname "$0")
echo "$*" >> "$D/log"
case "$*" in
"plugin marketplace list --json") if [ -f "$D/mk" ]; then echo '[{"name":"rgit"}]'; else echo '[]'; fi ;;
"plugin marketplace add "*) echo "$4" > "$D/mk" ;;
"plugin marketplace update rgit") ;;
"plugin marketplace remove rgit") rm -f "$D/mk" ;;
"plugin list --json") if [ -f "$D/ver" ]; then printf '[{"id":"rgit@rgit","version":"%s"}]\n' "$(cat "$D/ver")"; else echo '[]'; fi ;;
"plugin install rgit@rgit" | "plugin update rgit@rgit")
    sed -n 's/.*"version": "\(.*\)".*/\1/p' "$(cat "$D/mk")/plugins/rgit/.claude-plugin/plugin.json" > "$D/ver"
    # Only with FAKE_CLAUDE_HOME, a test's own home, does it record the install where Claude Code does.
    if [ -n "$FAKE_CLAUDE_HOME" ]; then
        mkdir -p "$FAKE_CLAUDE_HOME/.claude/plugins"
        printf '{"version":2,"plugins":{"rgit@rgit":[{"scope":"user","version":"%s"}]}}\n' "$(cat "$D/ver")" > "$FAKE_CLAUDE_HOME/.claude/plugins/installed_plugins.json"
    fi ;;
"plugin uninstall rgit@rgit")
    rm -f "$D/ver"
    if [ -n "$FAKE_CLAUDE_HOME" ]; then rm -f "$FAKE_CLAUDE_HOME/.claude/plugins/installed_plugins.json"; fi ;;
*) echo "fake claude: unexpected $*" >&2; exit 1 ;;
esac
