#!/usr/bin/env bash
# A small repo with history, a branch ahead of main and uncommitted work.
set -euo pipefail
g() { git -c user.name=t -c user.email=t@t "$@"; }
git init -q -b main
printf 'print("hi")\n' > app.py
printf '# Demo\n' > README.md
g add . && g commit -q -m "feat: add app"
printf 'def add(a, b):\n    return a + b\n' > calc.py
g add calc.py && g commit -q -m "feat: add calc"
git checkout -q -b topic
printf 'def sub(a, b):\n    return a - b\n' >> calc.py
g commit -q -am "feat: add sub"
printf 'print("hello")\n' > app.py
printf '# Demo\n\nRun app.py.\n' > README.md
printf 'notes\n' > notes.txt
