//! `git mailsplit` and `git mailinfo` over am's mbox splitter and mail parser.

use std::path::PathBuf;

use crate::GitError;
use crate::am::{MailOpts, is_from_line, mailinfo as parse_mail, split_mbox};

/// `git mailsplit`'s options.
pub struct MailsplitOpts {
    pub dir: PathBuf,
    /// The number of the mail before the first one written (`-f`).
    pub start: usize,
    /// Digits in the file names (`-d`).
    pub prec: usize,
    pub bare: bool,
    pub keep_cr: bool,
    pub mboxrd: bool,
}

/// Split each mbox (`None` is `stdin`) or Maildir into numbered files in
/// `o.dir`; returns how many, as git prints it.
pub fn mailsplit(
    sources: &[Option<PathBuf>],
    stdin: &[u8],
    o: &MailsplitOpts,
) -> Result<usize, GitError> {
    let mut nr = o.start;
    let mut write = |mail: &[u8]| -> Result<(), GitError> {
        nr += 1;
        let name = o.dir.join(format!("{nr:0width$}", width = o.prec));
        std::fs::write(&name, mail).map_err(|e| {
            GitError::Other(format!("cannot open output file {}: {e}", name.display()))
        })
    };
    for src in sources {
        let (data, bare) = match src {
            Some(p) if p.is_dir() => {
                let mut files: Vec<PathBuf> = ["cur", "new"]
                    .iter()
                    .filter_map(|sub| std::fs::read_dir(p.join(sub)).ok())
                    .flat_map(|d| d.flatten().map(|e| e.path()))
                    .collect();
                files.sort();
                for f in files {
                    for mail in split_mbox(&std::fs::read(f)?, o.keep_cr, o.mboxrd, true) {
                        write(&mail)?;
                    }
                }
                continue;
            }
            Some(p) => {
                let data = std::fs::read(p)
                    .map_err(|e| GitError::Other(format!("cannot stat {}: {e}", p.display())))?;
                if data.iter().all(u8::is_ascii_whitespace) {
                    return Err(GitError::Other(format!("empty mbox: '{}'", p.display())));
                }
                (data, o.bare)
            }
            None => (stdin.to_vec(), o.bare),
        };
        let first = data
            .split(|&b| b == b'\n')
            .find(|l| !l.iter().all(u8::is_ascii_whitespace));
        if let Some(first) = first {
            let mut line = first.to_vec();
            line.push(b'\n');
            if !bare && !is_from_line(&line) {
                return Err(GitError::Other("corrupt mailbox".to_owned()));
            }
        }
        for mail in split_mbox(&data, o.keep_cr, o.mboxrd, false) {
            write(&mail)?;
        }
    }
    Ok(nr - o.start)
}

/// `git mailinfo`'s options.
pub struct MailinfoOpts {
    pub keep_subject: bool,
    pub keep_non_patch: bool,
    pub message_id: bool,
    pub scissors: bool,
}

/// Parse a mail as `git mailinfo` does: git's header summary (`Author:`,
/// `Email:`, `Subject:`, `Date:`), the message and the patch.
pub fn mailinfo(mail: &[u8], o: &MailinfoOpts) -> (String, String, Vec<u8>) {
    let m = parse_mail(
        mail,
        &MailOpts {
            keep_subject: o.keep_subject,
            keep_non_patch: o.keep_non_patch,
            message_id: o.message_id,
            scissors: o.scissors,
        },
    );
    let mut info = String::new();
    if !m.author.is_empty() || !m.email.is_empty() {
        info.push_str(&format!("Author: {}\nEmail: {}\n", m.author, m.email));
    }
    if !m.subject.is_empty() {
        for line in m.subject.split('\n') {
            info.push_str(&format!("Subject: {line}\n"));
        }
    }
    if !m.date.is_empty() {
        info.push_str(&format!("Date: {}\n", m.date));
    }
    info.push('\n');
    (info, m.msg, m.patch)
}
