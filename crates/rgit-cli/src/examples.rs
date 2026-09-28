type Entry = (&'static str, &'static [&'static str]);

const EXAMPLES: &[Entry] = &[
    ("status", &["rgit status", "rgit status --toon"]),
    (
        "log",
        &[
            "rgit log --limit 50",
            "rgit log main -- src/lib.rs",
            "rgit log main..feature",
            "rgit log --author alice --since 2024-01-01",
            "rgit log --grep fix --no-merges -p",
            "rgit log --follow --stat -- src/lib.rs",
            "rgit log --oneline --graph --all",
            "rgit log --oneline -S parse_date -- src",
            "rgit log -3 --format='%h %an %ar %s' --date=iso",
        ],
    ),
    (
        "diff",
        &[
            "rgit diff",
            "rgit diff --cached -- src",
            "rgit diff main...HEAD --name-status",
            "rgit diff v1 HEAD --patch -U1 -w -- src",
            "rgit diff --quiet && echo clean",
            "rgit diff main --name-only --diff-filter=A",
            "rgit diff --no-index a.txt b.txt --patch",
        ],
    ),
    (
        "show",
        &[
            "rgit show HEAD",
            "rgit show <rev> --patch",
            "rgit show <rev> --name-only",
            "rgit show <rev> --pretty=fuller --stat",
            "rgit show HEAD~1:src/lib.rs",
        ],
    ),
    (
        "blame",
        &[
            "rgit blame src/lib.rs",
            "rgit blame src/lib.rs -L 10,20",
            "rgit blame v1 -- src/lib.rs",
            "rgit blame --porcelain -L 10,20 src/lib.rs",
        ],
    ),
    ("refs", &["rgit refs"]),
    (
        "rev-parse",
        &[
            "rgit rev-parse HEAD",
            "rgit rev-parse --short HEAD",
            "rgit rev-parse --abbrev-ref HEAD",
            "rgit rev-parse --show-toplevel --show-prefix",
            "rgit rev-parse --symbolic-full-name @{u}",
        ],
    ),
    (
        "ls-files",
        &[
            "rgit ls-files",
            "rgit ls-files -s src",
            "rgit ls-files -o --exclude-standard",
        ],
    ),
    (
        "ls-tree",
        &["rgit ls-tree HEAD", "rgit ls-tree -r -l HEAD src/"],
    ),
    (
        "cat-file",
        &[
            "rgit cat-file -p HEAD",
            "rgit cat-file -p HEAD:src/lib.rs",
            "rgit cat-file -t <rev>",
            "rgit cat-file --batch-check < ids.txt",
        ],
    ),
    (
        "show-ref",
        &[
            "rgit show-ref --heads",
            "rgit show-ref --verify refs/heads/main",
        ],
    ),
    (
        "for-each-ref",
        &[
            "rgit for-each-ref refs/heads",
            "rgit for-each-ref --sort=-committerdate --format='%(refname:short) %(subject)'",
            "rgit for-each-ref --merged main refs/heads",
        ],
    ),
    (
        "rev-list",
        &[
            "rgit rev-list --count HEAD",
            "rgit rev-list main..HEAD",
            "rgit rev-list -n 1 --all",
            "rgit rev-list HEAD -- src/lib.rs",
        ],
    ),
    (
        "merge-base",
        &[
            "rgit merge-base main HEAD",
            "rgit merge-base --is-ancestor main HEAD",
        ],
    ),
    ("reflog", &["rgit reflog", "rgit reflog show main -n 10"]),
    (
        "shortlog",
        &["rgit shortlog -sn", "rgit shortlog -sne --all"],
    ),
    (
        "grep",
        &[
            "rgit grep -n TODO",
            "rgit grep -i -e foo -e bar -- src",
            "rgit grep pattern HEAD~5",
            "rgit grep -n -C2 --heading TODO",
        ],
    ),
    (
        "check-ignore",
        &[
            "rgit check-ignore -v target/out.o",
            "rgit check-ignore --stdin < paths.txt",
        ],
    ),
    (
        "var",
        &[
            "rgit var GIT_AUTHOR_IDENT",
            "rgit var GIT_EDITOR",
            "rgit var -l",
        ],
    ),
    (
        "symbolic-ref",
        &[
            "rgit symbolic-ref HEAD",
            "rgit symbolic-ref --short HEAD",
            "rgit symbolic-ref HEAD refs/heads/main",
        ],
    ),
    ("count-objects", &["rgit count-objects -v"]),
    (
        "stage",
        &[
            "rgit stage src/lib.rs",
            "rgit stage src/lib.rs --hunk 42",
            "rgit stage src/lib.rs --hunk 10,42",
            "rgit stage src/lib.rs --hunk 42 --lines 0,2",
        ],
    ),
    (
        "unstage",
        &[
            "rgit unstage src/lib.rs",
            "rgit unstage src/lib.rs --hunk 42",
        ],
    ),
    ("stage-all", &["rgit stage-all"]),
    ("unstage-all", &["rgit unstage-all"]),
    (
        "add",
        &[
            "rgit add .",
            "rgit add src/ '*.md'",
            "rgit add -A",
            "rgit add -u",
            "rgit add -p",
            "rgit add -n .",
        ],
    ),
    (
        "discard",
        &[
            "rgit discard src/lib.rs",
            "rgit discard src/lib.rs --hunk 10",
        ],
    ),
    (
        "restore",
        &[
            "rgit restore src/lib.rs",
            "rgit restore --staged src/lib.rs",
            "rgit restore --source HEAD~1 src/lib.rs",
            "rgit restore --ours src/lib.rs",
        ],
    ),
    (
        "resolve",
        &[
            "rgit resolve src/lib.rs --ours",
            "rgit resolve src/lib.rs --theirs",
        ],
    ),
    (
        "commit",
        &[
            "rgit commit -m \"<message>\"",
            "rgit commit -a -m \"<message>\"",
            "rgit commit src/lib.rs -m \"<message>\"",
            "rgit commit -m \"<subject>\" -m \"<body>\"",
            "rgit commit --amend --no-edit",
            "rgit commit --fixup <rev>",
            "rgit commit -C <rev> --reset-author",
        ],
    ),
    ("extend", &["rgit extend"]),
    (
        "reword",
        &[
            "rgit reword -m \"<message>\"",
            "rgit reword -m \"<message>\" <rev>",
        ],
    ),
    ("uncommit", &["rgit uncommit", "rgit uncommit 2"]),
    (
        "squash",
        &[
            "rgit squash",
            "rgit squash <rev>",
            "rgit squash --from <rev>",
        ],
    ),
    (
        "move",
        &[
            "rgit move <rev> --before <rev>",
            "rgit move <rev> --after <rev>",
        ],
    ),
    (
        "split",
        &[
            "rgit split src/lib.rs src/main.rs",
            "rgit split --rev <rev> src/lib.rs",
        ],
    ),
    ("prune", &["rgit prune", "rgit prune --dry-run"]),
    ("next", &["rgit next"]),
    ("prev", &["rgit prev"]),
    (
        "fetch",
        &[
            "rgit fetch",
            "rgit fetch --all",
            "rgit fetch --remote origin --prune",
            "rgit fetch origin main --depth 1",
            "rgit fetch --dry-run",
            "rgit fetch --unshallow",
            "rgit fetch --prune --prune-tags",
            "rgit fetch --multiple origin upstream",
        ],
    ),
    (
        "pull",
        &[
            "rgit pull",
            "rgit pull --rebase",
            "rgit pull --ff-only",
            "rgit pull origin main",
            "rgit pull --autostash --rebase",
            "rgit pull --squash origin feature",
        ],
    ),
    ("sync", &["rgit sync"]),
    ("submit", &["rgit submit"]),
    (
        "push",
        &[
            "rgit push",
            "rgit push --set-upstream",
            "rgit push --force-with-lease",
            "rgit push origin feature",
            "rgit push origin local:remote",
            "rgit push origin --delete feature",
            "rgit push origin :v1",
            "rgit push --all --dry-run",
            "rgit push --follow-tags",
            "rgit push --atomic origin main v1",
            "rgit push --porcelain origin main",
        ],
    ),
    (
        "ls-remote",
        &[
            "rgit ls-remote",
            "rgit ls-remote --heads origin",
            "rgit ls-remote --tags https://github.com/example/repo.git 'v1.*'",
            "rgit ls-remote --symref origin HEAD",
        ],
    ),
    (
        "checkout",
        &[
            "rgit checkout main",
            "rgit checkout -b <new_branch>",
            "rgit checkout -",
            "rgit checkout <rev> -- src/lib.rs",
            "rgit checkout -f <branch>",
            "rgit checkout --theirs -- src/lib.rs",
        ],
    ),
    (
        "switch",
        &[
            "rgit switch main",
            "rgit switch -c <new_branch>",
            "rgit switch -",
            "rgit switch --detach <rev>",
            "rgit switch -m <branch>",
            "rgit switch --orphan <new_branch>",
        ],
    ),
    (
        "merge",
        &[
            "rgit merge <branch>",
            "rgit merge <branch> --no-ff",
            "rgit merge <branch> --squash",
            "rgit merge <branch> -m \"<message>\"",
            "rgit merge <a> <b>",
            "rgit merge <branch> -s ours",
            "rgit merge <branch> --log --no-stat",
            "rgit merge <branch> --allow-unrelated-histories",
            "rgit merge --continue",
            "rgit merge --abort",
        ],
    ),
    (
        "rebase",
        &[
            "rgit rebase main",
            "rgit rebase --onto <newbase> <upstream>",
            "rgit rebase --autosquash main",
            "rgit rebase --exec \"cargo test\" main",
            "rgit rebase --root",
            "rgit rebase -r --committer-date-is-author-date main",
            "rgit rebase --continue",
            "rgit rebase --show-current-patch",
        ],
    ),
    ("undo", &["rgit undo"]),
    ("redo", &["rgit redo"]),
    ("oplog", &["rgit oplog"]),
    ("smartlog", &["rgit smartlog", "rgit sl"]),
    (
        "bisect",
        &[
            "rgit bisect start HEAD main",
            "rgit bisect good",
            "rgit bisect bad",
            "rgit bisect run cargo test",
            "rgit bisect reset",
        ],
    ),
    (
        "bisect start",
        &[
            "rgit bisect start",
            "rgit bisect start <bad> <good>",
            "rgit bisect start <bad> <good> -- src/",
            "rgit bisect start --term-new fixed --term-old broken",
        ],
    ),
    ("bisect bad", &["rgit bisect bad", "rgit bisect bad <rev>"]),
    (
        "bisect good",
        &["rgit bisect good", "rgit bisect good <a> <b>"],
    ),
    ("bisect new", &["rgit bisect new"]),
    ("bisect old", &["rgit bisect old"]),
    (
        "bisect skip",
        &["rgit bisect skip", "rgit bisect skip <a>..<b>"],
    ),
    (
        "bisect reset",
        &["rgit bisect reset", "rgit bisect reset <rev>"],
    ),
    ("bisect log", &["rgit bisect log > bisect.log"]),
    ("bisect replay", &["rgit bisect replay bisect.log"]),
    (
        "bisect run",
        &[
            "rgit bisect run cargo test",
            "rgit bisect run ./check.sh --quick",
        ],
    ),
    (
        "bisect visualize",
        &["rgit bisect visualize", "rgit bisect view"],
    ),
    (
        "bisect terms",
        &["rgit bisect terms", "rgit bisect terms --term-good"],
    ),
    (
        "reset",
        &[
            "rgit reset HEAD~1",
            "rgit reset --hard <rev>",
            "rgit reset src/lib.rs",
            "rgit reset <rev> -- src/lib.rs",
            "rgit reset --merge",
        ],
    ),
    (
        "cherry-pick",
        &[
            "rgit cherry-pick <rev>",
            "rgit cherry-pick <rev> --no-commit",
            "rgit cherry-pick <a> <b>",
            "rgit cherry-pick main~3..main -x",
            "rgit cherry-pick <merge> -m 1",
            "rgit cherry-pick <rev> -s --empty=drop",
            "rgit cherry-pick --continue",
            "rgit cherry-pick --abort",
            "rgit cherry-pick --quit",
        ],
    ),
    (
        "revert",
        &[
            "rgit revert <rev>",
            "rgit revert <rev> --no-commit",
            "rgit revert HEAD~2..HEAD",
            "rgit revert <merge> -m 1",
            "rgit revert <rev> --reference",
            "rgit revert --continue",
        ],
    ),
    (
        "tag",
        &[
            "rgit tag",
            "rgit tag v1.0.0 -m \"<message>\"",
            "rgit tag v1.0.0 <rev>",
            "rgit tag -l \"v1.*\" -n",
            "rgit tag --sort=-v:refname --format='%(refname:short) %(creatordate:short)'",
            "rgit tag -s v1.0.0 -F notes.txt",
            "rgit tag --contains <rev>",
            "rgit tag -d v1.0.0 v1.0.1",
        ],
    ),
    ("absorb", &["rgit absorb"]),
    (
        "config",
        &[
            "rgit config user.email",
            "rgit config user.email me@example.com",
            "rgit config --global pull.rebase true",
            "rgit config --unset core.pager",
            "rgit config --list --show-origin",
            "rgit config --get-regexp '^remote\\.'",
            "rgit config --type=bool --default false core.bare",
            "rgit config --file .gitmodules --list",
            "rgit config --rename-section branch.old branch.new",
            "rgit config get --all --show-names remote.origin.fetch",
            "rgit config set --all core.pager less",
        ],
    ),
    (
        "apply",
        &[
            "rgit apply fix.patch",
            "rgit apply --check fix.patch",
            "rgit apply --cached fix.patch",
            "rgit apply -R fix.patch",
            "rgit apply --3way fix.patch",
            "rgit apply --reject -p2 --directory=vendor/lib fix.patch",
            "rgit apply --stat --summary --numstat fix.patch",
        ],
    ),
    ("notes", &["rgit notes", "rgit notes show HEAD"]),
    ("notes list", &["rgit notes list"]),
    ("notes show", &["rgit notes show", "rgit notes show <rev>"]),
    (
        "notes add",
        &[
            "rgit notes add -m \"<note>\"",
            "rgit notes add <rev> -m \"<note>\" --force",
            "rgit notes add -F notes.txt <rev>",
        ],
    ),
    ("notes append", &["rgit notes append -m \"<more>\""]),
    (
        "notes remove",
        &[
            "rgit notes remove <rev>",
            "rgit notes remove --ignore-missing a b",
        ],
    ),
    ("notes copy", &["rgit notes copy <from> <to>"]),
    ("notes edit", &["rgit notes edit <rev>"]),
    ("notes prune", &["rgit notes prune -n"]),
    ("notes merge", &["rgit notes merge -s union origin"]),
    ("notes get-ref", &["rgit notes --ref review get-ref"]),
    (
        "update-ref",
        &[
            "rgit update-ref refs/heads/topic <rev>",
            "rgit update-ref refs/heads/topic <new> <old>",
            "rgit update-ref -d refs/heads/topic",
            "printf 'start\\nupdate refs/heads/a <new> <old>\\ncommit\\n' | rgit update-ref --stdin",
            "rgit update-ref --create-reflog refs/backup/main main",
        ],
    ),
    (
        "hash-object",
        &[
            "rgit hash-object src/lib.rs",
            "rgit hash-object -w --stdin",
            "rgit hash-object --stdin --path src/lib.rs",
            "rgit hash-object --no-filters -t blob file.bin",
        ],
    ),
    (
        "format-patch",
        &[
            "rgit format-patch -1",
            "rgit format-patch main -o patches",
            "rgit format-patch main..topic --stdout",
            "rgit format-patch main --cover-letter --thread -v2 --to list@example.com",
            "rgit format-patch -3 --rfc --base=auto --subject-prefix=\"PATCH net\"",
        ],
    ),
    (
        "am",
        &[
            "rgit am patches/*.patch",
            "rgit am --continue",
            "rgit am --abort",
            "rgit am -3 -s --committer-date-is-author-date series.mbox",
            "rgit am --show-current-patch=diff",
        ],
    ),
    (
        "archive",
        &[
            "rgit archive -o release.tar.gz",
            "rgit archive v1.0 --prefix project/ -o project.zip",
            "rgit archive HEAD src > src.tar",
            "rgit archive -9 --add-file=VERSION --format=tar.gz -o dist.tgz v1.0",
            "rgit archive --remote ../other -o other.zip",
            "rgit archive -l",
        ],
    ),
    (
        "gc",
        &[
            "rgit gc",
            "rgit gc --prune=now",
            "rgit gc --aggressive --cruft",
        ],
    ),
    (
        "fsck",
        &[
            "rgit fsck",
            "rgit fsck --unreachable --no-reflogs",
            "rgit fsck --lost-found",
            "rgit fsck --name-objects --full",
        ],
    ),
    (
        "repack",
        &["rgit repack -a -d", "rgit repack -a -d -b --cruft"],
    ),
    (
        "cherry",
        &["rgit cherry", "rgit cherry -v origin/main topic"],
    ),
    (
        "bundle",
        &[
            "rgit bundle create repo.bundle --all",
            "rgit bundle verify repo.bundle",
        ],
    ),
    (
        "bundle create",
        &[
            "rgit bundle create repo.bundle --all",
            "rgit bundle create update.bundle v1.0..main",
        ],
    ),
    ("bundle verify", &["rgit bundle verify update.bundle"]),
    (
        "difftool",
        &[
            "rgit difftool -y -t meld",
            "rgit difftool --cached",
            "rgit difftool -d main topic",
        ],
    ),
    (
        "mergetool",
        &["rgit mergetool", "rgit mergetool -t vimdiff -- src/lib.rs"],
    ),
    (
        "range-diff",
        &[
            "rgit range-diff main topic-v1 topic-v2",
            "rgit range-diff -s main..topic@{1} main..topic",
            "rgit range-diff topic@{u}...topic",
        ],
    ),
    (
        "request-pull",
        &[
            "rgit request-pull origin/main https://example.com/me/repo.git topic",
            "rgit request-pull -p v1.0 origin",
        ],
    ),
    ("bundle list-heads", &["rgit bundle list-heads repo.bundle"]),
    ("bundle unbundle", &["rgit bundle unbundle update.bundle"]),
    ("pack-refs", &["rgit pack-refs --all"]),
    (
        "maintenance",
        &["rgit maintenance run --task=gc", "rgit maintenance start"],
    ),
    (
        "maintenance run",
        &[
            "rgit maintenance run",
            "rgit maintenance run --task=commit-graph --task=loose-objects",
        ],
    ),
    ("maintenance start", &["rgit maintenance start"]),
    ("maintenance stop", &["rgit maintenance stop"]),
    ("maintenance register", &["rgit maintenance register"]),
    (
        "maintenance unregister",
        &["rgit maintenance unregister --force"],
    ),
    ("clean", &["rgit clean", "rgit clean --dry-run"]),
    (
        "rm",
        &[
            "rgit rm src/lib.rs",
            "rgit rm src/lib.rs --cached",
            "rgit rm -n -r src/",
        ],
    ),
    (
        "mv",
        &[
            "rgit mv src/old.rs src/new.rs",
            "rgit mv -n src/old.rs src/new.rs",
        ],
    ),
    (
        "describe",
        &[
            "rgit describe",
            "rgit describe HEAD --tags",
            "rgit describe --tags --abbrev=0",
            "rgit describe --contains <rev>",
        ],
    ),
    (
        "init",
        &[
            "rgit init",
            "rgit init <path> -b main",
            "rgit init --bare --shared=group <path>.git",
            "rgit init --separate-git-dir ../<path>.git",
        ],
    ),
    (
        "clone",
        &[
            "rgit clone https://github.com/example/repo.git",
            "rgit clone https://github.com/example/repo.git repo-dir --depth 1",
            "rgit clone --bare https://github.com/example/repo.git",
            "rgit clone --recurse-submodules -o upstream https://github.com/example/repo.git",
            "rgit clone --single-branch -b main https://github.com/example/repo.git",
            "rgit clone --mirror https://github.com/example/repo.git",
            "rgit clone --filter=blob:none https://github.com/example/repo.git",
        ],
    ),
    (
        "submodule",
        &["rgit submodule", "rgit submodule update --init --recursive"],
    ),
    ("submodule status", &["rgit submodule status --recursive"]),
    (
        "submodule add",
        &[
            "rgit submodule add https://github.com/example/lib.git vendor/lib",
            "rgit submodule add -b main https://github.com/example/lib.git",
        ],
    ),
    ("submodule init", &["rgit submodule init"]),
    (
        "submodule update",
        &[
            "rgit submodule update --init --recursive",
            "rgit submodule update --remote vendor/lib",
        ],
    ),
    ("submodule sync", &["rgit submodule sync --recursive"]),
    (
        "submodule deinit",
        &[
            "rgit submodule deinit vendor/lib",
            "rgit submodule deinit -f --all",
        ],
    ),
    (
        "submodule foreach",
        &["rgit submodule foreach 'echo $sm_path $sha1'"],
    ),
    ("submodule summary", &["rgit submodule summary"]),
    (
        "submodule set-url",
        &["rgit submodule set-url vendor/lib https://github.com/fork/lib.git"],
    ),
    (
        "submodule set-branch",
        &[
            "rgit submodule set-branch -b main vendor/lib",
            "rgit submodule set-branch -d vendor/lib",
        ],
    ),
    ("submodule absorbgitdirs", &["rgit submodule absorbgitdirs"]),
    ("git", &["rgit git status", "rgit git log --oneline -5"]),
    ("mcp", &["rgit mcp"]),
    (
        "serve",
        &[
            "rgit serve",
            "rgit serve --port 9000",
            "rgit serve --root ~/code --clone-base https://git.example.dev",
        ],
    ),
    (
        "branch",
        &[
            "rgit branch",
            "rgit branch -vv",
            "rgit branch --merged main",
            "rgit branch --sort=-committerdate --format='%(refname:short) %(upstream:short)'",
            "rgit branch <branch> <start>",
            "rgit branch -c <branch> <new_branch>",
            "rgit branch -d <branch> <branch>",
            "rgit branch -u origin/<branch>",
        ],
    ),
    (
        "branch create",
        &[
            "rgit branch create <branch>",
            "rgit branch create <branch> origin/<branch>",
        ],
    ),
    ("branch checkout", &["rgit branch checkout <branch>"]),
    (
        "branch delete",
        &[
            "rgit branch delete <branch> <branch>",
            "rgit branch delete <branch> --force",
        ],
    ),
    ("branch rename", &["rgit branch rename <old> <new>"]),
    (
        "branch prune",
        &["rgit branch prune", "rgit branch prune main"],
    ),
    (
        "stash",
        &[
            "rgit stash",
            "rgit stash -u",
            "rgit stash push -m \"<message>\"",
        ],
    ),
    (
        "stash push",
        &[
            "rgit stash push -m \"<message>\" --include-untracked",
            "rgit stash push -- src/lib.rs",
            "rgit stash push --staged -m \"<message>\"",
            "rgit stash push -p",
        ],
    ),
    (
        "stash create",
        &["rgit stash create", "rgit stash create \"<message>\""],
    ),
    (
        "stash store",
        &["rgit stash store -m \"<message>\" <commit>"],
    ),
    (
        "stash pop",
        &["rgit stash pop", "rgit stash pop stash@{1} --index"],
    ),
    ("stash apply", &["rgit stash apply"]),
    ("stash drop", &["rgit stash drop 0"]),
    (
        "stash list",
        &[
            "rgit stash list",
            "rgit stash list --format=\"%gd %cr %gs\" -n 5",
        ],
    ),
    (
        "stash show",
        &[
            "rgit stash show",
            "rgit stash show -p stash@{1}",
            "rgit stash show -u --name-only",
        ],
    ),
    ("stash branch", &["rgit stash branch <branch>"]),
    ("stash clear", &["rgit stash clear"]),
    (
        "remote",
        &[
            "rgit remote",
            "rgit remote -v",
            "rgit remote add origin https://github.com/example/repo.git",
        ],
    ),
    (
        "remote add",
        &[
            "rgit remote add origin https://github.com/example/repo.git",
            "rgit remote add -f -t main upstream https://github.com/example/repo.git",
        ],
    ),
    ("remote remove", &["rgit remote remove origin"]),
    (
        "remote set-url",
        &[
            "rgit remote set-url origin https://github.com/example/repo.git",
            "rgit remote set-url --push origin https://github.com/fork/repo.git",
            "rgit remote set-url --add --push origin https://github.com/mirror/repo.git",
        ],
    ),
    ("remote get-url", &["rgit remote get-url origin"]),
    ("remote rename", &["rgit remote rename origin upstream"]),
    (
        "remote prune",
        &["rgit remote prune origin", "rgit remote prune -n origin"],
    ),
    (
        "remote show",
        &["rgit remote show origin", "rgit remote show -n"],
    ),
    (
        "remote update",
        &["rgit remote update", "rgit remote update -p <group>"],
    ),
    (
        "remote set-head",
        &[
            "rgit remote set-head origin -a",
            "rgit remote set-head origin main",
        ],
    ),
    (
        "remote set-branches",
        &["rgit remote set-branches --add origin <branch>"],
    ),
    ("worktree", &["rgit worktree"]),
    (
        "worktree add",
        &[
            "rgit worktree add ../<path> <branch>",
            "rgit worktree add -b <new_branch> ../<path> origin/main",
            "rgit worktree add --detach ../<path> <rev>",
            "rgit worktree add -B <branch> --lock --reason \"<reason>\" ../<path>",
            "rgit worktree add --orphan -b <new_branch> ../<path>",
        ],
    ),
    (
        "worktree list",
        &["rgit worktree list -v", "rgit worktree list --porcelain"],
    ),
    (
        "worktree remove",
        &[
            "rgit worktree remove ../<path>",
            "rgit worktree remove <name> --force",
        ],
    ),
    (
        "worktree lock",
        &["rgit worktree lock ../<path> --reason \"<reason>\""],
    ),
    ("worktree unlock", &["rgit worktree unlock ../<path>"]),
    (
        "worktree move",
        &["rgit worktree move ../<path> ../<new_path>"],
    ),
    ("worktree prune", &["rgit worktree prune"]),
    (
        "worktree repair",
        &["rgit worktree repair", "rgit worktree repair ../<path>"],
    ),
    ("workspace", &["rgit workspace"]),
    ("workspace new", &["rgit workspace new <name>"]),
    ("workspace list", &["rgit workspace list"]),
    ("workspace remove", &["rgit workspace remove <name>"]),
    ("flow", &["rgit flow status"]),
    ("flow init", &["rgit flow init gitflow"]),
    ("flow start", &["rgit flow start <name>"]),
    ("flow finish", &["rgit flow finish"]),
    (
        "flow release",
        &[
            "rgit flow release 1.2.0",
            "rgit flow release 1.2.0 --finish",
        ],
    ),
    ("flow status", &["rgit flow status"]),
    (
        "forge",
        &["rgit forge --profile work whoami", "rgit forge pr list"],
    ),
    (
        "forge login",
        &[
            "rgit forge login github --token-stdin",
            "rgit forge login gitlab --host https://gitlab.example.com --token-stdin",
        ],
    ),
    ("forge auth", &["rgit forge auth list"]),
    ("forge auth list", &["rgit forge auth list"]),
    ("forge auth status", &["rgit forge auth status"]),
    ("forge whoami", &["rgit forge whoami github"]),
    ("forge logout", &["rgit forge logout github"]),
    ("forge repo", &["rgit forge repo view"]),
    (
        "forge repo view",
        &["rgit forge repo view", "rgit forge repo view owner/repo"],
    ),
    (
        "forge repo create",
        &[
            "rgit forge repo create <name>",
            "rgit forge repo create <name> --organization <org> --private",
        ],
    ),
    (
        "forge repo delete",
        &["rgit forge repo delete owner/repo --yes"],
    ),
    ("forge branch", &["rgit forge branch list"]),
    (
        "forge branch list",
        &[
            "rgit forge branch list",
            "rgit forge branch list owner/repo",
        ],
    ),
    (
        "forge branch delete",
        &[
            "rgit forge branch delete <branch> --yes",
            "rgit forge branch delete <branch> owner/repo --yes",
        ],
    ),
    ("forge pr", &["rgit forge pr list"]),
    (
        "forge pr list",
        &[
            "rgit forge pr list",
            "rgit forge pr list owner/repo",
            "rgit forge pr list --page 2",
        ],
    ),
    (
        "forge pr create",
        &["rgit forge pr create --title \"<title>\" --head <branch> --base main"],
    ),
    (
        "forge pr close",
        &[
            "rgit forge pr close 42 --yes",
            "rgit forge pr close 42 owner/repo --yes",
        ],
    ),
    ("stack", &["rgit stack"]),
    ("stack new", &["rgit stack new <branch>"]),
    ("stack list", &["rgit stack list"]),
    ("stack restack", &["rgit stack restack"]),
    ("lanes", &["rgit lanes"]),
    ("lanes init", &["rgit lanes init"]),
    ("lanes off", &["rgit lanes off"]),
    ("lanes list", &["rgit lanes list"]),
    ("lanes new", &["rgit lanes new <lane>"]),
    (
        "lanes stack",
        &["rgit lanes stack <lane> --on <parent_lane>"],
    ),
    (
        "lanes assign",
        &[
            "rgit lanes assign <lane> src/lib.rs",
            "rgit lanes assign <lane> src/lib.rs --hunk 10",
        ],
    ),
    ("lanes unassign", &["rgit lanes unassign src/lib.rs"]),
    (
        "lanes commit",
        &["rgit lanes commit <lane> -m \"<message>\""],
    ),
    ("lanes rename", &["rgit lanes rename <old> <new>"]),
    ("lanes delete", &["rgit lanes delete <lane>"]),
    ("lanes push", &["rgit lanes push <lane>"]),
    ("lanes pr", &["rgit lanes pr <lane>"]),
    ("lanes restack", &["rgit lanes restack"]),
    ("skills", &["rgit skills list"]),
    ("skills list", &["rgit skills list"]),
    (
        "skills show",
        &["rgit skills show", "rgit skills show --reference"],
    ),
    (
        "skills install",
        &[
            "rgit skills install --project",
            "rgit skills install --user --target claude",
        ],
    ),
    ("index", &["rgit index status"]),
    (
        "index build",
        &["rgit index build", "rgit index build --root ~/code"],
    ),
    (
        "index search",
        &[
            "rgit index search \"<query>\"",
            "rgit index search \"<query>\" --limit 5",
        ],
    ),
    ("index status", &["rgit index status"]),
    ("index code", &["rgit index code \"<query>\""]),
    ("hooks", &["rgit hooks install", "rgit hooks status"]),
    (
        "hooks install",
        &[
            "rgit hooks install",
            "rgit hooks install --app codex",
            "rgit hooks install --user --app all",
        ],
    ),
    ("hooks status", &["rgit hooks status"]),
];

fn render(lines: &[&str]) -> String {
    let mut out = String::from("Examples:\n");
    for line in lines {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.pop();
    out
}

/// The usage examples for a space-joined subcommand path.
pub fn lookup(path: &str) -> Option<&'static [&'static str]> {
    EXAMPLES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, lines)| *lines)
}

fn apply_at(cmd: clap::Command, prefix: &str) -> clap::Command {
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    names.into_iter().fold(cmd, |cmd, name| {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix} {name}")
        };
        cmd.mut_subcommand(&name, |sub| {
            let sub = apply_at(sub, &path);
            match lookup(&path) {
                Some(lines) => sub.after_help(render(lines)),
                None => sub,
            }
        })
    })
}

/// Attach usage examples to every subcommand's help.
pub fn apply(cmd: clap::Command) -> clap::Command {
    apply_at(cmd, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn walk(cmd: &clap::Command, prefix: &str, out: &mut Vec<(String, bool)>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name == "help" {
                continue;
            }
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix} {name}")
            };
            out.push((path.clone(), sub.get_after_help().is_some()));
            walk(sub, &path, out);
        }
    }

    #[test]
    fn every_table_key_exists() {
        let cmd = crate::cli::Cli::command();
        let mut found = Vec::new();
        walk(&cmd, "", &mut found);
        let paths: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
        for (path, _) in EXAMPLES {
            assert!(
                paths.contains(path),
                "table key {path:?} not found in the command tree"
            );
        }
    }

    #[test]
    fn every_subcommand_has_after_help() {
        let cmd = apply(crate::cli::Cli::command());
        let mut found = Vec::new();
        walk(&cmd, "", &mut found);
        assert!(!found.is_empty());
        for (path, has_help) in found {
            assert!(has_help, "missing after_help for {path:?}");
        }
    }
}
