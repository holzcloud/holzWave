//! The Markdown renderer of the chat against a hostile corpus, and the fixture the page tests replay.
//!
//! A model reply is untrusted text. The core renders it with `render_markdown` (pulldown-cmark, then an allow-list
//! sanitiser) and the page copies the answer through a second allow-list of its own. This file proves the core half:
//! every case of the corpus is live (its marker is in the raw HTML of the parser, so the sanitiser has something to
//! remove), the renderer's output has no forbidden element, attribute or link scheme, and the golden
//! `app/tests/fixtures/ai_xss.json` is exactly what the renderer and the parser produce. The page tests read that
//! fixture and replay both forms of every case, the cleaned HTML and the raw HTML a failed sanitiser would deliver.
//!
//! No network, no engine, no Zig: `cargo test --locked --offline -p holzwave-core --no-default-features --test
//! ai_xss_golden`. `UPDATE_FIXTURES=1` rewrites the fixture.

mod common;

use holzwave_core::preview::{MAX_MARKDOWN, render_markdown};
use pulldown_cmark::{Options, Parser, html};
use serde_json::{Value, json};

/// What the renderer must do with the marker of a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// The marker is in the raw HTML and must be gone from the renderer's output.
    Removed,
    /// The marker must survive in the output (as escaped text) and the page must show the `visible` text.
    Text,
    /// The renderer may keep the marker; the page decides what to do with it.
    Open,
}

impl Expect {
    fn word(self) -> &'static str {
        match self {
            Expect::Removed => "removed",
            Expect::Text => "text",
            Expect::Open => "open",
        }
    }
}

/// One hostile Markdown source. Owned strings, so a case can be built with `format!` and repeats.
struct Case {
    id: String,
    class: &'static str,
    expect: Expect,
    /// A piece of the raw HTML that proves the case is live: it is in the parser's output.
    marker: String,
    source: String,
    /// The text the page must show for a `Text` case; empty for the others.
    visible: String,
}

fn case(id: &str, class: &'static str, expect: Expect, marker: &str, source: &str) -> Case {
    Case { id: id.into(), class, expect, marker: marker.into(), source: source.into(), visible: String::new() }
}

/// The corpus. A hidden character in a later case is written as a braced escape and is never typed.
fn corpus() -> Vec<Case> {
    vec![case(
        "script-block",
        "script",
        Expect::Removed,
        "<script",
        "A paragraph before.\n\n<script>window.__XSS = 1</script>\n\n<img src=x onerror=\"window.__XSS = 1\">\n",
    )]
}

/// What the sanitiser has to clean: the parser's own HTML for `source`, with the four options of the renderer.
fn raw_html(source: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_FOOTNOTES;
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(source, options));
    out
}

/// The elements of the sanitiser, in its order. The unit test of the page compares this list with the core's source and
/// with the page's own allow-list.
const ALLOWED_ELEMENTS: [&str; 29] = [
    "a", "p", "br", "hr", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "code", "em", "strong",
    "del", "table", "thead", "tbody", "tr", "th", "td", "sup", "div", "span", "input",
];

/// Elements that load something, run something or change the page around them. None may be in the output.
const FORBIDDEN_ELEMENTS: [&str; 31] = [
    "script", "style", "link", "meta", "base", "iframe", "frame", "frameset", "object", "embed", "applet", "img", "picture",
    "source", "video", "audio", "track", "canvas", "svg", "math", "form", "select", "textarea", "template", "noscript",
    "portal", "dialog", "marquee", "bgsound", "xmp", "plaintext",
];

/// Attributes that load, submit, restyle, clobber a global name or take the focus. None may be in the output.
const FORBIDDEN_ATTRIBUTES: [&str; 20] = [
    "style", "src", "srcset", "srcdoc", "action", "formaction", "background", "poster", "ping", "data", "id", "name", "content",
    "http-equiv", "xlink:href", "is", "autofocus", "tabindex", "contenteditable", "accesskey",
];

/// Attribute names that start with one of these are event handlers.
const FORBIDDEN_PREFIXES: [&str; 1] = ["on"];

/// Schemes no link may carry. A link may have `http`, `https` or `mailto`, or no scheme at all.
const FORBIDDEN_SCHEMES: [&str; 11] =
    ["javascript", "data", "vbscript", "file", "holzfile", "blob", "about", "view-source", "ws", "wss", "ftp"];

/// The only schemes a link may have.
const LINK_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// One tag of the renderer's output.
#[derive(Debug, PartialEq, Eq)]
struct Tag {
    name: String,
    closing: bool,
    attributes: Vec<(String, String)>,
}

/// Every tag of `html`. The serializer escapes text, so every `<` starts a tag; names are lower-cased and attribute values
/// are read between double quotes.
fn tags_of(html: &str) -> Vec<Tag> {
    let bytes = html.as_bytes();
    let mut tags = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        i += 1;
        let closing = bytes.get(i) == Some(&b'/');
        if closing {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/' {
            i += 1;
        }
        let name = html[start..i].to_ascii_lowercase();
        let mut attributes = Vec::new();
        loop {
            while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] == b'>' {
                i += 1;
                break;
            }
            let from = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' && bytes[i] != b'>' && bytes[i] != b'/' {
                i += 1;
            }
            let attribute = html[from..i].to_ascii_lowercase();
            let mut value = String::new();
            if bytes.get(i) == Some(&b'=') {
                i += 1;
                if bytes.get(i) == Some(&b'"') {
                    i += 1;
                    let from = i;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += 1;
                    }
                    value = html[from..i.min(bytes.len())].to_string();
                    i += 1;
                } else {
                    let from = i;
                    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                        i += 1;
                    }
                    value = html[from..i].to_string();
                }
            }
            attributes.push((attribute, value));
        }
        tags.push(Tag { name, closing, attributes });
    }
    tags
}

/// The scheme of a link address: the text before the first colon when that text has only letters, digits, plus, minus and
/// dot. A browser drops tabs, line feeds and carriage returns inside an address and ignores leading control characters and
/// spaces, so they are dropped here too. `None` for an address with no scheme (a relative one).
fn scheme_of(href: &str) -> Option<String> {
    let cleaned: String = href.chars().filter(|c| !c.is_ascii_control() && *c != ' ').collect();
    let (head, _) = cleaned.split_once(':')?;
    let scheme = head.to_ascii_lowercase();
    let plain = !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    plain.then_some(scheme)
}

/// One line for every forbidden element, attribute or prefix in `html`, and for every `href` whose scheme is not one of the
/// three. An `href` with no scheme is relative and passes.
fn problems(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    for tag in tags_of(html) {
        if tag.closing {
            continue;
        }
        if FORBIDDEN_ELEMENTS.contains(&tag.name.as_str()) {
            found.push(format!("forbidden element <{}>", tag.name));
        } else if !ALLOWED_ELEMENTS.contains(&tag.name.as_str()) {
            found.push(format!("element <{}> is not on the allow-list", tag.name));
        }
        for (name, value) in &tag.attributes {
            if FORBIDDEN_ATTRIBUTES.contains(&name.as_str()) {
                found.push(format!("forbidden attribute {name} on <{}>", tag.name));
            }
            if FORBIDDEN_PREFIXES.iter().any(|p| name.starts_with(p)) {
                found.push(format!("event handler attribute {name} on <{}>", tag.name));
            }
            if name.starts_with("xlink:") || name.starts_with("xmlns") {
                found.push(format!("namespace attribute {name} on <{}>", tag.name));
            }
            if name == "href" {
                if tag.name != "a" {
                    found.push(format!("href on <{}>", tag.name));
                }
                if let Some(scheme) = scheme_of(value) {
                    if FORBIDDEN_SCHEMES.contains(&scheme.as_str()) || !LINK_SCHEMES.contains(&scheme.as_str()) {
                        found.push(format!("link scheme {scheme}: on <{}>", tag.name));
                    }
                }
            }
            if name == "class" && !(tag.name == "code" && value.starts_with("language-")) {
                found.push(format!("class {value:?} on <{}>", tag.name));
            }
        }
    }
    found
}

/// The member `core_html` of a case: the renderer's answer, or `null` when it refused.
fn core_html(case: &Case) -> Option<String> {
    render_markdown(&case.source)
}

/// The whole fixture as a JSON value; every object has sorted keys because `serde_json` is built without `preserve_order`.
fn golden() -> Value {
    let cases: Vec<Value> = corpus()
        .iter()
        .map(|c| {
            json!({
                "class": c.class,
                "core_html": core_html(c),
                "expect": c.expect.word(),
                "id": c.id,
                "marker": c.marker,
                "raw_html": raw_html(&c.source),
                "source": c.source,
                "visible": c.visible,
            })
        })
        .collect();
    json!({
        "cases": cases,
        "elements": ALLOWED_ELEMENTS,
        "forbidden": {
            "attribute_prefixes": FORBIDDEN_PREFIXES,
            "attributes": FORBIDDEN_ATTRIBUTES,
            "elements": FORBIDDEN_ELEMENTS,
            "schemes": FORBIDDEN_SCHEMES,
        },
        "limits": { "max_markdown": MAX_MARKDOWN },
        "version": 1,
    })
}

/// A character that is invisible, a separator, a direction control, a tag character or a control character other than the
/// line feed: it is never written raw into the fixture, so the fixture is safe to read and to review.
fn is_hidden(c: char) -> bool {
    let n = c as u32;
    (c.is_control() && c != '\n')
        || (0x7f..=0x9f).contains(&n)
        || n == 0xa0
        || n == 0x1680
        || (0x2000..=0x200f).contains(&n)
        || (0x2028..=0x202f).contains(&n)
        || (0x205f..=0x206f).contains(&n)
        || n == 0x3000
        || n == 0xfeff
        || (0xe000..=0xf8ff).contains(&n)
        || (0xe0000..=0xe007f).contains(&n)
}

/// `text` with every hidden character turned into a JSON escape: four lower-case hex digits, a surrogate pair above U+FFFF.
fn escape_hidden(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_hidden(c) {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The text of the fixture: pretty JSON, hidden characters escaped, one trailing line feed.
fn golden_text() -> String {
    let mut text = escape_hidden(&serde_json::to_string_pretty(&golden()).expect("json"));
    text.push('\n');
    text
}

/// Write the fixture with `UPDATE_FIXTURES=1`, else compare it with `text`.
fn write_or_compare(text: &str) {
    let path = common::fixtures_dir().join("ai_xss.json");
    if common::update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("dir")).expect("create fixtures dir");
        std::fs::write(&path, text).expect("write ai_xss.json");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1 and review", path.display()));
    assert!(on_disk == text, "ai_xss.json differs from what the renderer produces; run with UPDATE_FIXTURES=1 and review");
}

#[test]
fn the_corpus_is_live_every_case_has_its_marker_in_the_raw_html() {
    let cases = corpus();
    assert!(!cases.is_empty(), "the corpus is empty");
    for c in &cases {
        let raw = raw_html(&c.source);
        assert!(raw.contains(&c.marker), "{}: the marker {:?} is not in the raw HTML, so the case proves nothing: {raw}", c.id, c.marker);
        let cleaned = core_html(c);
        match c.expect {
            Expect::Removed => {
                if let Some(out) = &cleaned {
                    assert!(!out.contains(&c.marker), "{}: the marker {:?} survived the renderer: {out}", c.id, c.marker);
                }
            }
            Expect::Text => {
                let out = cleaned.as_deref().unwrap_or_default();
                assert!(out.contains(&c.marker), "{}: the marker {:?} must survive as text: {out}", c.id, c.marker);
                assert!(!c.visible.is_empty(), "{}: a text case names the text the page shows", c.id);
            }
            Expect::Open => {}
        }
    }
    let ids: std::collections::HashSet<&str> = cases.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len(), "ids are unique");
}

#[test]
fn the_renderer_output_has_no_forbidden_element_attribute_or_scheme() {
    // The scanner can fail: a synthetic output with a script element and a handler is flagged, and so is every kind of link.
    let flagged = problems("<p>x</p><script>1</script><p onerror=\"1\">y</p><a href=\"JaVa\tScript:1\">z</a><img src=\"a\">");
    for want in ["forbidden element <script>", "event handler attribute onerror", "link scheme javascript:", "forbidden element <img>", "forbidden attribute src"] {
        assert!(flagged.iter().any(|l| l.contains(want)), "the scanner missed {want}: {flagged:?}");
    }
    assert!(problems("<p><a href=\"data:text/html,x\">d</a></p>").iter().any(|l| l.contains("data")), "a data link is flagged");
    assert!(problems("<p><a href=\"ftp://example.org/\">d</a></p>").iter().any(|l| l.contains("ftp")), "any other scheme is flagged");
    assert!(problems("<p><a href=\"../notes.md\">r</a> <a href=\"https://example.org/\">h</a> <a href=\"mailto:a@example.org\">m</a></p>").is_empty());
    assert!(ALLOWED_ELEMENTS.iter().all(|e| !FORBIDDEN_ELEMENTS.contains(e)), "an element is both allowed and forbidden");
    assert_eq!(tags_of("<a href=\"x\" rel=\"y\">t</a><br>").len(), 3);

    for c in corpus() {
        let Some(out) = core_html(&c) else { continue };
        let found = problems(&out);
        assert!(found.is_empty(), "{}: {found:?}\n{out}", c.id);
    }
}

#[test]
fn the_golden_equals_what_the_renderer_produces() {
    write_or_compare(&golden_text());
}

#[test]
fn the_fixture_holds_no_raw_hidden_character() {
    let text = golden_text();
    let bad: Vec<char> = text.chars().filter(|c| is_hidden(*c)).collect();
    assert!(bad.is_empty(), "the fixture text has raw hidden characters: {bad:?}");
    // The writer turns them into escapes, and only the writer: a hidden character in a case survives as an escape.
    assert_eq!(escape_hidden("a\u{202e}b\u{e0041}c\u{0}d"), "a\\u202eb\\udb40\\udc41c\\u0000d");
    assert_eq!(escape_hidden("plain \u{e9} text\n"), "plain \u{e9} text\n");
}
