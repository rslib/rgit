type Entry = (&'static str, &'static [&'static str]);

const EXAMPLES: &[Entry] = &[
    (
        "status",
        &[
            "rgit status",
            "rgit status --toon",
            "rgit status --long",
            "rgit status -sb",
            "rgit status --porcelain=v2 -b --show-stash",
            "rgit status -vv -uall --ignored=matching",
        ],
    ),
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
            "rgit log --oneline --left-right --cherry-pick main...feature",
            "rgit log --since='last friday' --until=yesterday",
            "rgit log -g -5 --oneline",
            "rgit log -3 --format='%h %an %ar %s' --date=iso",
            "rgit log -L :parse_date:src/cli.rs --oneline",
            "rgit log --merges --cc --oneline",
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
            "rgit show <merge> --remerge-diff --format=medium",
            "rgit show <merge> -m --stat --oneline",
        ],
    ),
    (
        "blame",
        &[
            "rgit blame src/lib.rs",
            "rgit blame src/lib.rs -L 10,20",
            "rgit blame v1 -- src/lib.rs",
            "rgit blame --porcelain -L 10,20 src/lib.rs",
            "rgit blame -w -C -L :main src/lib.rs",
            "rgit blame --ignore-rev <rev> --date=short src/lib.rs",
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
            "rgit cat-file --textconv HEAD:doc.pdf",
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
            "rgit for-each-ref --format='%(refname:short) %(ahead-behind:main) %(*subject)'",
        ],
    ),
    (
        "rev-list",
        &[
            "rgit rev-list --count HEAD",
            "rgit rev-list main..HEAD",
            "rgit rev-list -n 1 --all",
            "rgit rev-list HEAD -- src/lib.rs",
            "rgit rev-list --count --left-right main...feature",
            "rgit rev-list --objects main..feature",
        ],
    ),
    (
        "merge-base",
        &[
            "rgit merge-base main HEAD",
            "rgit merge-base --is-ancestor main HEAD",
        ],
    ),
    (
        "reflog",
        &[
            "rgit reflog",
            "rgit reflog show main -n 10",
            "rgit reflog expire --expire=30.days.ago --all",
            "rgit reflog delete --rewrite HEAD@{2}",
        ],
    ),
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
            "rgit grep -e TODO --and --not -e FIXME",
            "rgit grep -W -n parse_args -- src",
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
        "stripspace",
        &["rgit stripspace < msg.txt", "rgit stripspace -s < msg.txt"],
    ),
    (
        "column",
        &["rgit branch | rgit column --mode=column,dense --width=80"],
    ),
    (
        "check-ref-format",
        &[
            "rgit check-ref-format refs/heads/topic",
            "rgit check-ref-format --branch @{-1}",
            "rgit check-ref-format --normalize --allow-onelevel //x",
        ],
    ),
    (
        "patch-id",
        &[
            "rgit log -p main..topic | rgit patch-id --stable",
            "rgit diff | rgit patch-id",
        ],
    ),
    (
        "name-rev",
        &[
            "rgit name-rev HEAD~3",
            "rgit name-rev --tags --name-only <rev>",
            "rgit log --format=%H | rgit name-rev --annotate-stdin",
        ],
    ),
    (
        "check-attr",
        &[
            "rgit check-attr -a src/lib.rs",
            "rgit check-attr text eol -- a.txt b.bin",
        ],
    ),
    (
        "diff-tree",
        &[
            "rgit diff-tree -r HEAD",
            "rgit diff-tree -p main feature -- src",
        ],
    ),
    (
        "diff-index",
        &[
            "rgit diff-index HEAD",
            "rgit diff-index --cached --name-only HEAD",
        ],
    ),
    (
        "diff-files",
        &["rgit diff-files", "rgit diff-files --quiet"],
    ),
    (
        "merge-tree",
        &[
            "rgit merge-tree --write-tree main feature",
            "rgit merge-tree --name-only main feature",
        ],
    ),
    (
        "merge-file",
        &[
            "rgit merge-file ours.txt base.txt theirs.txt",
            "rgit merge-file -p --diff3 -L a -L b -L c a b c",
        ],
    ),
    (
        "commit-tree",
        &[
            "rgit commit-tree 'HEAD^{tree}' -p HEAD -m 'Snapshot'",
            "rgit commit-tree $(rgit write-tree) -p main -p topic -F msg.txt",
        ],
    ),
    (
        "write-tree",
        &["rgit write-tree", "rgit write-tree --prefix=src/"],
    ),
    (
        "read-tree",
        &[
            "rgit read-tree HEAD",
            "rgit read-tree -m -u HEAD topic",
            "rgit read-tree -m base ours theirs",
            "rgit read-tree --prefix=vendor/lib/ lib-main",
        ],
    ),
    (
        "update-index",
        &[
            "rgit update-index --add new.txt",
            "rgit update-index --chmod=+x run.sh",
            "rgit update-index --add --cacheinfo 100644,<sha1>,path",
            "rgit update-index --assume-unchanged config.local",
            "rgit update-index --refresh",
        ],
    ),
    (
        "checkout-index",
        &[
            "rgit checkout-index -f src/lib.rs",
            "rgit checkout-index -a --prefix=/tmp/export/",
        ],
    ),
    (
        "mktree",
        &[
            "rgit ls-tree HEAD | rgit mktree",
            "rgit mktree --batch < trees.txt",
        ],
    ),
    ("mktag", &["rgit mktag < tag.txt"]),
    (
        "get-tar-commit-id",
        &["rgit get-tar-commit-id < release.tar"],
    ),
    (
        "fmt-merge-msg",
        &[
            "rgit fmt-merge-msg < .git/FETCH_HEAD",
            "rgit fmt-merge-msg --log=5 -F .git/FETCH_HEAD",
        ],
    ),
    (
        "mailsplit",
        &[
            "rgit mailsplit -o/tmp/mails series.mbox",
            "rgit mailsplit -b -d3 -f10 -o/tmp/mails --mboxrd inbox.mbox",
        ],
    ),
    (
        "mailinfo",
        &["rgit mailinfo msg.txt patch.diff < /tmp/mails/0001"],
    ),
    (
        "interpret-trailers",
        &[
            "rgit interpret-trailers --trailer 'Reviewed-by: A <a@x>' < msg.txt",
            "rgit interpret-trailers --in-place --where start --trailer Fixes=123 msg.txt",
            "rgit log -1 --format=%B | rgit interpret-trailers --parse",
        ],
    ),
    (
        "show-branch",
        &[
            "rgit show-branch",
            "rgit show-branch --more=5 main topic",
            "rgit show-branch --merge-base main topic",
            "rgit show-branch --reflog=4 main",
        ],
    ),
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
            "rgit add -i",
            "rgit add -e src/lib.rs",
            "rgit add --chmod=+x build.sh",
            "rgit add --renormalize .",
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
            "rgit restore --merge src/lib.rs",
            "rgit restore -p --source=HEAD~1 --staged --worktree",
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
            "rgit commit --dry-run -a",
            "rgit commit --short",
            "rgit commit -v -e -m \"<message>\"",
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
    (
        "prune",
        &[
            "rgit prune",
            "rgit prune --dry-run",
            "rgit prune -v --expire=2.weeks.ago",
        ],
    ),
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
            "rgit fetch --multiple -j 4 origin upstream",
            "rgit fetch --filter=blob:none origin",
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
            "rgit push --recurse-submodules=on-demand origin main",
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
            "rgit checkout --conflict=diff3 -- src/lib.rs",
            "rgit checkout -p <rev>",
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
            "rgit merge <a> <b> -e",
            "rgit merge <branch> --autostash",
            "rgit merge <branch> --into-name <other>",
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
            "rgit rebase --update-refs --autostash main",
            "GIT_SEQUENCE_EDITOR=\"sed -i.bak 1s/^pick/edit/\" rgit rebase -i main",
            "rgit rebase --continue",
            "rgit rebase --abort",
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
    ("bisect next", &["rgit bisect next"]),
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
            "rgit reset -p <rev>",
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
            "rgit cherry-pick <rev> --strategy=ours --cleanup=strip",
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
            "rgit tag --column=row,dense",
            "rgit tag -m \"<message>\" --trailer \"Reviewed-by: <name>\" v1.0.0",
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
            "rgit apply -C1 --ignore-whitespace fix.patch",
            "rgit apply -N new-files.patch",
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
            "rgit notes add --no-stripspace -F notes.txt <rev>",
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
    (
        "notes merge",
        &[
            "rgit notes merge -s union origin",
            "rgit notes merge origin",
            "rgit notes merge --commit",
            "rgit notes merge --abort",
        ],
    ),
    ("notes get-ref", &["rgit notes --ref review get-ref"]),
    (
        "update-ref",
        &[
            "rgit update-ref refs/heads/topic <rev>",
            "rgit update-ref refs/heads/topic <new> <old>",
            "rgit update-ref -d refs/heads/topic",
            "printf 'start\\nupdate refs/heads/a <new> <old>\\ncommit\\n' | rgit update-ref --stdin",
            "rgit update-ref --create-reflog refs/backup/main main",
            "printf 'symref-update refs/heads/alias refs/heads/main\\n' | rgit update-ref --stdin",
            "printf 'create refs/heads/a <rev>\\ncreate refs/heads/b <rev>\\n' | rgit update-ref --stdin --batch-updates",
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
            "rgit format-patch -2 --attach --notes --output=series.mbox",
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
            "rgit am -c -m --keep-cr ~/Maildir/patches",
            "rgit am --skip",
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
            "rgit archive -v --remote=git@example.com:me/repo.git -o repo.tar.gz v1.0",
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
        "verify-commit",
        &[
            "rgit verify-commit HEAD",
            "rgit verify-commit -v --raw HEAD",
        ],
    ),
    (
        "verify-tag",
        &["rgit verify-tag v1.0", "rgit verify-tag -v v1.0"],
    ),
    (
        "repack",
        &[
            "rgit repack -a -d",
            "rgit repack -a -d -b --cruft",
            "rgit repack -a -d -f --window=250 --depth=50",
        ],
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
            "rgit range-diff --creation-factor=80 --no-notes -U1 main v1 v2 -- src",
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
        "commit-graph",
        &[
            "rgit commit-graph write --reachable --changed-paths",
            "rgit commit-graph verify",
        ],
    ),
    (
        "commit-graph write",
        &[
            "rgit commit-graph write --reachable",
            "rgit commit-graph write --reachable --split --size-multiple=4",
            "rgit rev-list --all | rgit commit-graph write --stdin-commits --append",
        ],
    ),
    (
        "commit-graph verify",
        &[
            "rgit commit-graph verify",
            "rgit commit-graph verify --shallow",
        ],
    ),
    (
        "multi-pack-index",
        &[
            "rgit multi-pack-index write",
            "rgit multi-pack-index verify",
        ],
    ),
    (
        "multi-pack-index write",
        &[
            "rgit multi-pack-index write",
            "rgit multi-pack-index write --preferred-pack=pack-1234.pack",
        ],
    ),
    ("multi-pack-index verify", &["rgit multi-pack-index verify"]),
    ("multi-pack-index expire", &["rgit multi-pack-index expire"]),
    (
        "multi-pack-index repack",
        &[
            "rgit multi-pack-index repack --batch-size=0",
            "rgit multi-pack-index repack --batch-size=100m",
        ],
    ),
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
    (
        "maintenance start",
        &[
            "rgit maintenance start",
            "rgit maintenance start --scheduler=crontab",
        ],
    ),
    (
        "for-each-repo",
        &["rgit for-each-repo --config=maintenance.repo maintenance run --schedule=daily"],
    ),
    ("maintenance stop", &["rgit maintenance stop"]),
    ("maintenance register", &["rgit maintenance register"]),
    (
        "maintenance unregister",
        &["rgit maintenance unregister --force"],
    ),
    (
        "clean",
        &["rgit clean -n", "rgit clean -fd", "rgit clean -idx"],
    ),
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
            "rgit describe --all --dirty=-wip",
            "rgit describe --first-parent --exclude 'rc*'",
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
            "rgit clone --shallow-since=2024-01-01 https://github.com/example/repo.git",
            "rgit clone --reference ../other --dissociate https://github.com/example/repo.git",
            "rgit clone --sparse https://github.com/example/repo.git",
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
            "rgit branch --column",
            "rgit branch -v --abbrev=12 --color=always",
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
            "rgit stash list --oneline",
            "rgit stash list -p",
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
            "rgit worktree add --guess-remote ../<branch>",
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
