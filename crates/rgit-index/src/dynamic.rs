//! Dynamic tree-sitter grammars, nvim-style: for a language we do not bundle,
//! clone its grammar repo, compile `parser.c` (and any scanner) into a shared
//! library with the system C/C++ compiler, cache it, and load it via `dlopen`.
//! Needs `git` and a compiler at runtime; when either is missing the caller
//! falls back to line-window chunking. The grammar's own `queries/tags.scm`
//! drives definition extraction, exactly like the bundled languages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use tree_sitter::Language;

/// A file extension we can grammar-load on demand: the tree-sitter language name
/// (which is also the `tree_sitter_<name>` symbol) and its grammar repository.
/// Repos are chosen to have `src/parser.c` and `queries/tags.scm` at the root.
fn registry(ext: &str) -> Option<(&'static str, &'static str)> {
    Some(match ext {
        "rb" => ("ruby", "https://github.com/tree-sitter/tree-sitter-ruby"),
        "sh" | "bash" => ("bash", "https://github.com/tree-sitter/tree-sitter-bash"),
        "cs" => (
            "c_sharp",
            "https://github.com/tree-sitter/tree-sitter-c-sharp",
        ),
        "scala" | "sbt" => ("scala", "https://github.com/tree-sitter/tree-sitter-scala"),
        "ex" | "exs" => (
            "elixir",
            "https://github.com/elixir-lang/tree-sitter-elixir",
        ),
        "lua" => (
            "lua",
            "https://github.com/tree-sitter-grammars/tree-sitter-lua",
        ),
        "ml" | "mli" => ("ocaml", "https://github.com/tree-sitter/tree-sitter-ocaml"),
        _ => return None,
    })
}

/// The per-process cache of loaded (language, tags-query). A `None` marks a
/// language we already failed to build, so we do not retry every file.
type Cache = HashMap<String, Option<(Language, String)>>;
fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("rgit")
            .join("grammars");
    }
    std::env::temp_dir().join("rgit-grammars")
}

/// Resolve an extension to a dynamically-loaded grammar and its tags query, or
/// None when the extension is unknown or the grammar cannot be built/loaded.
pub(crate) fn language_for(ext: &str) -> Option<(Language, String)> {
    let (name, repo) = registry(ext)?;
    let mut guard = cache().lock().expect("grammar cache");
    if let Some(entry) = guard.get(name) {
        return entry.clone();
    }
    let built = build_and_load(name, repo);
    guard.insert(name.to_owned(), built.clone());
    built
}

fn build_and_load(name: &str, repo: &str) -> Option<(Language, String)> {
    let dir = cache_dir().join(name);
    let lib = dir.join(format!("parser.{}", std::env::consts::DLL_EXTENSION));
    let tags_path = dir.join("tags.scm");
    if !lib.exists() && build(name, repo, &dir, &lib, &tags_path).is_err() {
        return None;
    }
    let language = load(name, &lib)?;
    let tags = std::fs::read_to_string(&tags_path).unwrap_or_default();
    Some((language, tags))
}

fn run(cmd: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} failed"))
    }
}

/// Clone the grammar, compile it into `lib`, and copy its tags query. Compiles
/// `parser.c` and any `scanner.c`/`scanner.cc` to objects, then links a shared
/// library (with the C++ driver when there is a C++ scanner).
fn build(name: &str, repo: &str, dir: &Path, lib: &Path, tags_path: &Path) -> Result<(), String> {
    use std::ffi::OsStr;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let repo_dir = dir.join("repo");
    if !repo_dir.join("src").exists() {
        let _ = std::fs::remove_dir_all(&repo_dir);
        run(
            "git",
            &[
                OsStr::new("clone"),
                OsStr::new("--depth"),
                OsStr::new("1"),
                OsStr::new(repo),
                repo_dir.as_os_str(),
            ],
        )?;
    }
    let src = repo_dir.join("src");
    let parser = src.join("parser.c");
    if !parser.exists() {
        return Err("no src/parser.c".to_owned());
    }
    let inc = format!("-I{}", src.display());
    let mut objects: Vec<PathBuf> = Vec::new();
    let mut cpp = false;

    let parser_o = dir.join("parser.o");
    run(
        "cc",
        &[
            OsStr::new("-c"),
            OsStr::new("-O2"),
            OsStr::new("-fPIC"),
            OsStr::new(&inc),
            parser.as_os_str(),
            OsStr::new("-o"),
            parser_o.as_os_str(),
        ],
    )?;
    objects.push(parser_o);

    let scanner_cc = src.join("scanner.cc");
    let scanner_c = src.join("scanner.c");
    if scanner_cc.exists() {
        cpp = true;
        let o = dir.join("scanner.o");
        run(
            "c++",
            &[
                OsStr::new("-c"),
                OsStr::new("-O2"),
                OsStr::new("-fPIC"),
                OsStr::new(&inc),
                scanner_cc.as_os_str(),
                OsStr::new("-o"),
                o.as_os_str(),
            ],
        )?;
        objects.push(o);
    } else if scanner_c.exists() {
        let o = dir.join("scanner.o");
        run(
            "cc",
            &[
                OsStr::new("-c"),
                OsStr::new("-O2"),
                OsStr::new("-fPIC"),
                OsStr::new(&inc),
                scanner_c.as_os_str(),
                OsStr::new("-o"),
                o.as_os_str(),
            ],
        )?;
        objects.push(o);
    }

    let driver = if cpp { "c++" } else { "cc" };
    let mut link: Vec<&OsStr> = vec![OsStr::new("-shared")];
    for o in &objects {
        link.push(o.as_os_str());
    }
    link.push(OsStr::new("-o"));
    link.push(lib.as_os_str());
    run(driver, &link)?;

    let q = repo_dir.join("queries").join("tags.scm");
    if q.exists() {
        let _ = std::fs::copy(&q, tags_path);
    }
    let _ = name;
    Ok(())
}

/// Load a compiled grammar and wrap its `tree_sitter_<name>` symbol as a
/// [`Language`]. The library is leaked so the language stays valid for the
/// process lifetime.
fn load(name: &str, lib: &Path) -> Option<Language> {
    unsafe {
        let library = libloading::Library::new(lib).ok()?;
        let symbol = format!("tree_sitter_{name}");
        let func: libloading::Symbol<unsafe extern "C" fn() -> *const ()> =
            library.get(symbol.as_bytes()).ok()?;
        let language_fn = tree_sitter_language::LanguageFn::from_raw(*func);
        let language = Language::new(language_fn);
        std::mem::forget(library);
        Some(language)
    }
}
