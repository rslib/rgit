//! Rendered previews for prose formats (Markdown, reStructuredText), the way
//! GitHub/GitLab show a README. Output is sanitized with `ammonia`, so repo
//! content can never inject scripts into the viewer.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};

use crate::highlight;

/// Sanitize rendered HTML while keeping `class` attributes, so syntax-highlight
/// classes survive. `class` is not executable; ammonia still strips scripts,
/// event handlers, and other unsafe markup.
fn sanitize(html: &str) -> String {
    ammonia::Builder::default()
        .add_generic_attributes(["class"])
        .clean(html)
        .to_string()
}

fn ext(path: &str) -> String {
    path.rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// True when a file has a preview (its source can also still be viewed).
pub fn is_renderable(path: &str) -> bool {
    matches!(
        ext(path).as_str(),
        "md" | "markdown" | "mkd" | "mdown" | "rst" | "rest"
    )
}

/// Render a previewable file to sanitized HTML, or None if it is not
/// previewable or rendering fails (the caller then shows the source).
pub fn render(path: &str, text: &str) -> Option<String> {
    match ext(path).as_str() {
        "md" | "markdown" | "mkd" | "mdown" => Some(render_markdown(text)),
        "rst" | "rest" => render_rst(text),
        _ => None,
    }
}

fn render_markdown(text: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);

    // Replace each fenced code block with syntect-highlighted HTML.
    let mut events = Vec::new();
    let mut code_lang: Option<String> = None;
    let mut code = String::new();
    for ev in Parser::new_ext(text, opts) {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                code.clear();
                code_lang = Some(match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                });
            }
            Event::End(TagEnd::CodeBlock) => {
                let lang = code_lang.take().unwrap_or_default();
                events.push(Event::Html(highlight::code_block_html(&lang, &code).into()));
            }
            Event::Text(t) if code_lang.is_some() => code.push_str(&t),
            other => events.push(other),
        }
    }
    let mut unsafe_html = String::new();
    html::push_html(&mut unsafe_html, events.into_iter());
    sanitize(&unsafe_html)
}

fn render_rst(text: &str) -> Option<String> {
    let doc = rst_parser::parse(text).ok()?;
    let mut buf = Vec::new();
    rst_renderer::render_html(&doc, &mut buf, false).ok()?;
    Some(sanitize(&String::from_utf8_lossy(&buf)))
}
