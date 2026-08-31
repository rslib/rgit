//! The status buffer's content as a typed tree.
//!
//! This crate maps git data ([`rgit_git::RepoStatus`]) into a [`Section`] tree
//! carrying semantic [`Style`] roles and stable ids, with no rendering or
//! interaction concerns. The view layer wraps it in an interactive buffer.

mod build;
mod content;
mod style;

pub use build::{
    DIFF_GUTTER_COLS, GlyphMode, SyntaxColors, build, build_blame, build_commit, build_diff,
    build_log, build_refs, diff_line_file_line, highlight_code, highlight_file,
    build_remotes, build_worktrees, glyph, glyph_mode, set_glyph_mode, set_side_by_side,
    set_syntax_colors, unicode,
};
pub use content::{NodeKind, RefTarget, Section, SectionId, Target};
pub use style::{Span, Style};

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_git::{Commit, Head, RepoStatus, StatusCode, StatusEntry};

    fn entry(path: &str, index: StatusCode, worktree: StatusCode) -> StatusEntry {
        StatusEntry {
            path: path.into(),
            orig_path: None,
            index,
            worktree,
        }
    }

    fn find<'a>(sections: &'a [Section], id: &str) -> Option<&'a Section> {
        sections.iter().find(|s| s.id == id)
    }

    #[test]
    fn a_modified_file_lands_in_unstaged_with_a_stable_id() {
        let status = RepoStatus {
            head: Head::default(),
            entries: vec![entry(
                "src/main.rs",
                StatusCode::Unmodified,
                StatusCode::Modified,
            )],
            recent: vec![],
            ..Default::default()
        };
        let sections = build(&status);
        let unstaged = find(&sections, "unstaged").expect("unstaged section");
        assert!(unstaged.is_foldable());
        assert_eq!(unstaged.children.len(), 1);
        assert_eq!(unstaged.children[0].id, "unstaged/src/main.rs");
    }

    #[test]
    fn a_file_staged_and_unstaged_appears_in_both_sections() {
        let status = RepoStatus {
            head: Head::default(),
            entries: vec![entry("x.rs", StatusCode::Modified, StatusCode::Modified)],
            recent: vec![Commit {
                short_id: "abc1234".into(),
                summary: "init".into(),
                when: "just now".into(),
                refs: Vec::new(),
                unpushed: false,
            }],
            ..Default::default()
        };
        let sections = build(&status);
        assert_eq!(find(&sections, "unstaged").unwrap().children.len(), 1);
        assert_eq!(find(&sections, "staged").unwrap().children.len(), 1);
        assert!(find(&sections, "recent").is_some());
    }

    #[test]
    fn a_files_hunks_and_lines_become_child_nodes() {
        use rgit_git::{DiffLine, FileDiff, Hunk, LineOrigin};

        let status = RepoStatus {
            head: Head::default(),
            entries: vec![entry("a.rs", StatusCode::Unmodified, StatusCode::Modified)],
            unstaged: vec![FileDiff {
                path: "a.rs".into(),
                old_path: None,
                binary: false,
                hunks: vec![Hunk {
                    header: "@@ -1,2 +1,2 @@".into(),
                    new_start: 1,
                    lines: vec![
                        DiffLine {
                            origin: LineOrigin::Removed,
                            text: "old".into(),
                        },
                        DiffLine {
                            origin: LineOrigin::Added,
                            text: "new".into(),
                        },
                    ],
                }],
            }],
            ..Default::default()
        };

        let sections = build(&status);
        let file = &find(&sections, "unstaged").unwrap().children[0];
        assert_eq!(file.kind, NodeKind::File);
        assert_eq!(file.children.len(), 1, "one hunk");
        let hunk = &file.children[0];
        assert_eq!(hunk.kind, NodeKind::Hunk);
        assert_eq!(hunk.children.len(), 2, "two diff lines");
        assert_eq!(hunk.children[0].kind, NodeKind::DiffLine);
        assert_eq!(hunk.id, "unstaged/a.rs#0");
    }

    #[test]
    fn a_clean_tree_shows_an_info_line() {
        let status = RepoStatus::default();
        let sections = build(&status);
        assert!(find(&sections, "info").is_some());
    }

    #[test]
    fn an_untracked_file_folds_open_to_its_added_content() {
        use rgit_git::{DiffLine, FileDiff, Hunk, LineOrigin};

        // An untracked file: worktree=Untracked, and its content arrives as an
        // all-added diff in the unstaged list (see the backend's status()).
        let status = RepoStatus {
            head: Head::default(),
            entries: vec![entry(
                "new.rs",
                StatusCode::Unmodified,
                StatusCode::Untracked,
            )],
            unstaged: vec![FileDiff {
                path: "new.rs".into(),
                old_path: None,
                binary: false,
                hunks: vec![Hunk {
                    header: "@@ -0,0 +1,2 @@".into(),
                    new_start: 1,
                    lines: vec![
                        DiffLine {
                            origin: LineOrigin::Added,
                            text: "one".into(),
                        },
                        DiffLine {
                            origin: LineOrigin::Added,
                            text: "two".into(),
                        },
                    ],
                }],
            }],
            ..Default::default()
        };

        let sections = build(&status);
        let untracked = find(&sections, "untracked").expect("untracked section");
        assert!(untracked.is_foldable());
        let file = &untracked.children[0];
        assert_eq!(file.kind, NodeKind::File);
        assert_eq!(file.id, "untracked/new.rs");
        // The regression: the untracked file must fold open to its hunk/lines,
        // not be a dead leaf with no diff (which is what happened before).
        assert_eq!(file.children.len(), 1, "one hunk under the untracked file");
        assert_eq!(file.children[0].children.len(), 2, "two added lines");
    }
}
