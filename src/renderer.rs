//! AST-to-HTML renderer for RST and Markdown documents.

use crate::directives::{Directive, DirectiveRegistry};
use crate::document::{DocumentContent, MarkdownContent, MarkdownNode, RstContent, RstNode};
use crate::roles::{Role, RoleRegistry};
use regex::Regex;
use std::collections::HashMap;
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;

/// HTML renderer that converts parsed AST to HTML.
pub struct HtmlRenderer {
    directive_registry: DirectiveRegistry,
    role_registry: RoleRegistry,
    /// Map of document paths to their titles (e.g., "intro" -> "Introduction")
    document_titles: HashMap<String, String>,
    /// Syntax definitions for code highlighting
    syntax_set: SyntaxSet,
    /// Theme for code highlighting
    theme_set: ThemeSet,
    /// Name of the theme to use for highlighting
    theme_name: String,
}

impl Default for HtmlRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl HtmlRenderer {
    /// Create a new HTML renderer with default directive and role registries.
    pub fn new() -> Self {
        Self {
            directive_registry: DirectiveRegistry::new(),
            role_registry: RoleRegistry::new(),
            document_titles: HashMap::new(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            theme_name: "base16-ocean.dark".to_string(),
        }
    }

    /// Set the syntax highlighting theme.
    /// Available themes: "InspiredGitHub", "Solarized (dark)", "Solarized (light)",
    /// "base16-ocean.dark", "base16-eighties.dark", "base16-mocha.dark", "base16-ocean.light"
    pub fn set_theme(&mut self, theme_name: &str) {
        if self.theme_set.themes.contains_key(theme_name) {
            self.theme_name = theme_name.to_string();
        }
    }

    /// Highlight code with syntax highlighting, falling back to plain text if language is unknown.
    fn highlight_code(&self, code: &str, language: Option<&str>) -> String {
        let theme = &self.theme_set.themes[&self.theme_name];

        // Try to find a syntax for the language
        let syntax = language
            .and_then(|lang| {
                // Try exact match first
                self.syntax_set.find_syntax_by_token(lang)
                    // Then try by extension
                    .or_else(|| self.syntax_set.find_syntax_by_extension(lang))
            })
            // Fall back to plain text
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        // Generate highlighted HTML
        match highlighted_html_for_string(code, &self.syntax_set, syntax, theme) {
            Ok(html) => html,
            Err(_) => {
                // Fallback to plain code block if highlighting fails
                let escaped = html_escape::encode_text(code);
                format!("<pre><code>{}</code></pre>", escaped)
            }
        }
    }

    /// Register a document title for use in toctree rendering.
    /// The path should be without the .rst extension (e.g., "intro" or "tutorial/getting-started").
    pub fn register_document_title(&mut self, path: &str, title: &str) {
        self.document_titles.insert(path.to_string(), title.to_string());
    }

    /// Look up a document title by path. Returns None if not registered.
    pub fn get_document_title(&self, path: &str) -> Option<&String> {
        self.document_titles.get(path)
    }

    /// Render document content to HTML.
    pub fn render_document_content(&self, content: &DocumentContent) -> String {
        match content {
            DocumentContent::RestructuredText(rst) => self.render_rst(rst),
            DocumentContent::Markdown(md) => self.render_markdown(md),
            DocumentContent::PlainText(text) => {
                format!("<p>{}</p>", html_escape::encode_text(text))
            }
        }
    }

    /// Render RST content to HTML.
    /// Wraps content in hierarchical section tags based on heading levels.
    pub fn render_rst(&self, content: &RstContent) -> String {
        let mut html = String::new();
        let mut open_sections: Vec<usize> = Vec::new(); // Stack of open section levels

        for node in &content.ast {
            // Check if this is a title and handle section nesting
            if let RstNode::Title { level, text, .. } = node {
                let level = (*level).min(6).max(1);

                // Close sections that are at the same level or deeper
                while let Some(&open_level) = open_sections.last() {
                    if open_level >= level {
                        html.push_str("</section>\n");
                        open_sections.pop();
                    } else {
                        break;
                    }
                }

                // Open a new section for this heading
                let plain_text = extract_plain_text_for_slug(text);
                let slug = slugify(&plain_text);
                html.push_str(&format!("<section id=\"{}\">\n", slug));
                open_sections.push(level);
            }

            html.push_str(&self.render_rst_node(node));
            html.push('\n');
        }

        // Close any remaining open sections
        for _ in open_sections {
            html.push_str("</section>\n");
        }

        html
    }

    /// Render a single RST node to HTML.
    fn render_rst_node(&self, node: &RstNode) -> String {
        match node {
            RstNode::Title { text, level, .. } => {
                // Extract plain text for slug generation (strips RST markup)
                let plain_text = extract_plain_text_for_slug(text);
                let slug = slugify(&plain_text);
                let level = (*level).min(6).max(1);
                // Process inline markup in titles (including roles)
                let rendered_text = self.render_rst_inline(text);
                // Add headerlink (¶ symbol) like Sphinx does
                // Note: id is on the parent <section> tag, not the heading
                format!(
                    "<h{level}>{text}<a class=\"headerlink\" href=\"#{slug}\" title=\"Link to this heading\">¶</a></h{level}>",
                    level = level,
                    slug = slug,
                    text = rendered_text
                )
            }

            RstNode::Paragraph { content, .. } => {
                let rendered = self.render_rst_inline(content);
                format!("<p>{}</p>", rendered)
            }

            RstNode::CodeBlock {
                language, content, ..
            } => self.highlight_code(content, language.as_deref()),

            RstNode::List {
                items,
                ordered,
                ..
            } => {
                let items_html: String = items
                    .iter()
                    .map(|item| {
                        // Check if item has nested content (contains newlines)
                        if item.contains('\n') {
                            let parts: Vec<&str> = item.split('\n').collect();
                            let term = self.render_rst_inline(parts[0]);
                            let nested_items: String = parts[1..]
                                .iter()
                                .map(|nested| format!("<li><p>{}</p></li>", self.render_rst_inline(nested)))
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!(
                                "<li><dl class=\"simple\">\n<dt>{}</dt><dd><ul>\n{}\n</ul>\n</dd>\n</dl></li>",
                                term, nested_items
                            )
                        } else {
                            format!("<li>{}</li>", self.render_rst_inline(item))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                // Use class="simple" for unordered lists like Sphinx does
                if *ordered {
                    format!("<ol>\n{}\n</ol>", items_html)
                } else {
                    format!("<ul class=\"simple\">\n{}\n</ul>", items_html)
                }
            }

            RstNode::Table { headers, rows, .. } => {
                let mut html = String::from("<table>\n");

                // Render header
                if !headers.is_empty() {
                    html.push_str("<thead>\n<tr>\n");
                    for header in headers {
                        html.push_str(&format!(
                            "<th>{}</th>\n",
                            html_escape::encode_text(header)
                        ));
                    }
                    html.push_str("</tr>\n</thead>\n");
                }

                // Render body
                if !rows.is_empty() {
                    html.push_str("<tbody>\n");
                    for row in rows {
                        html.push_str("<tr>\n");
                        for cell in row {
                            html.push_str(&format!(
                                "<td>{}</td>\n",
                                html_escape::encode_text(cell)
                            ));
                        }
                        html.push_str("</tr>\n");
                    }
                    html.push_str("</tbody>\n");
                }

                html.push_str("</table>");
                html
            }

            RstNode::Directive {
                name,
                args,
                options,
                content,
                line,
            } => {
                // Handle toctree specially since it needs access to document titles
                if name == "toctree" {
                    return self.render_toctree(options, content);
                }

                // Convert to Directive struct for processing
                let directive = Directive {
                    name: name.clone(),
                    arguments: args.clone(),
                    options: options.clone(),
                    content: content.lines().map(String::from).collect(),
                    line_number: *line,
                    source_file: String::new(),
                };

                match self.directive_registry.process_directive(&directive) {
                    Ok(html) => html,
                    Err(_) => format!("<!-- Error processing directive: {} -->", name),
                }
            }

            RstNode::LinkTarget { name, .. } => {
                // Render as an invisible anchor that can be linked to
                format!("<span id=\"{}\"></span>", html_escape::encode_text(name))
            }

            RstNode::BlockQuote { content, .. } => {
                // Render block quote with inline RST markup processing
                let rendered_content = self.render_rst_inline(content);
                format!("<blockquote>\n<p>{}</p>\n</blockquote>", rendered_content)
            }

            RstNode::DefinitionList { items, .. } => {
                let mut html = String::from("<dl class=\"simple\">\n");
                for item in items {
                    let rendered_term = self.render_rst_inline(&item.term);
                    let rendered_def = self.render_rst_inline(&item.definition);
                    html.push_str(&format!(
                        "<dt>{}</dt><dd><p>{}</p>\n</dd>\n",
                        rendered_term, rendered_def
                    ));
                }
                html.push_str("</dl>");
                html
            }
        }
    }

    /// Render a toctree directive with document title lookup.
    fn render_toctree(&self, options: &HashMap<String, String>, content: &str) -> String {
        let caption = options.get("caption");
        let hidden = options.contains_key("hidden");

        // Parse document entries from content
        let entries: Vec<&str> = content
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && !s.starts_with(':'))
            .collect();

        let mut html = String::new();

        // Start wrapper div
        if hidden {
            html.push_str("<div class=\"toctree-wrapper\" style=\"display: none;\">\n");
        } else {
            html.push_str("<div class=\"toctree-wrapper\">\n");
        }

        // Add caption if present
        if let Some(caption_text) = caption {
            html.push_str(&format!(
                "<p class=\"caption\"><span class=\"caption-text\">{}</span></p>\n",
                html_escape::encode_text(caption_text)
            ));
        }

        // Generate the list of links
        if !entries.is_empty() {
            html.push_str("<ul>\n");
            for entry in entries {
                // Handle entries with explicit titles: "Title <path>"
                let (title, path) = if let Some(angle_pos) = entry.find('<') {
                    if entry.ends_with('>') {
                        let title = entry[..angle_pos].trim();
                        let path = &entry[angle_pos + 1..entry.len() - 1];
                        (Some(title.to_string()), path.to_string())
                    } else {
                        (None, entry.to_string())
                    }
                } else {
                    (None, entry.to_string())
                };

                // Determine display title:
                // 1. Explicit title from "Title <path>" syntax
                // 2. Look up from document_titles registry
                // 3. Fall back to path
                let display_title = if let Some(explicit_title) = title {
                    explicit_title
                } else if let Some(registered_title) = self.document_titles.get(&path) {
                    registered_title.clone()
                } else {
                    path.clone()
                };

                // Convert path to .html link
                let href = format!("{}.html", path);

                html.push_str(&format!(
                    "<li class=\"toctree-l1\"><a class=\"reference internal\" href=\"{}\">{}</a></li>\n",
                    html_escape::encode_text(&href),
                    html_escape::encode_text(&display_title)
                ));
            }
            html.push_str("</ul>\n");
        }

        html.push_str("</div>");
        html
    }

    /// Render inline RST markup (bold, italic, code, roles, references).
    pub fn render_rst_inline(&self, text: &str) -> String {
        // Process roles FIRST on unescaped text to preserve angle brackets in "text <target>" format
        // We use a placeholder to protect the role output from subsequent escaping
        let role_re = Regex::new(r":([a-zA-Z][a-zA-Z0-9_:-]*):`([^`]+)`").unwrap();
        let mut role_replacements: Vec<String> = Vec::new();

        let result_with_placeholders = role_re
            .replace_all(text, |caps: &regex::Captures| {
                let role_name = &caps[1];
                let role_content = &caps[2];

                // Parse role content for "text <target>" format
                let (display_text, target) = if let Some(angle_pos) = role_content.find('<') {
                    if role_content.ends_with('>') {
                        let display_text = role_content[..angle_pos].trim();
                        let target = &role_content[angle_pos + 1..role_content.len() - 1];
                        (Some(display_text.to_string()), target.to_string())
                    } else {
                        (None, role_content.to_string())
                    }
                } else {
                    (None, role_content.to_string())
                };

                let role = Role {
                    name: role_name.to_string(),
                    target,
                    text: display_text,
                    line_number: 0,
                    source_file: String::new(),
                };

                let html = match self.role_registry.process_role(&role) {
                    Ok(html) => html,
                    Err(_) => format!("<!-- Unknown role: {} -->", role_name),
                };

                // Store the HTML and return a placeholder
                let placeholder = format!("\x00ROLE{}\x00", role_replacements.len());
                role_replacements.push(html);
                placeholder
            })
            .to_string();

        // Process references on unescaped text: `text`_ or `text <URL>`_
        let ref_re = Regex::new(r"`([^`]+)`_").unwrap();
        let result_with_placeholders = ref_re
            .replace_all(&result_with_placeholders, |caps: &regex::Captures| {
                let ref_text = &caps[1];

                // Check for external link format: `text <URL>`_
                let html = if let Some(angle_pos) = ref_text.rfind('<') {
                    if ref_text.ends_with('>') {
                        // External link with explicit URL
                        let display_text = ref_text[..angle_pos].trim();
                        let url = &ref_text[angle_pos + 1..ref_text.len() - 1];
                        format!(
                            "<a class=\"reference external\" href=\"{}\">{}</a>",
                            html_escape::encode_text(url),
                            html_escape::encode_text(display_text)
                        )
                    } else {
                        // Malformed, treat as internal reference
                        let anchor = slugify(ref_text);
                        format!(
                            "<a class=\"reference internal\" href=\"#{}\">{}</a>",
                            anchor,
                            html_escape::encode_text(ref_text)
                        )
                    }
                } else {
                    // Internal reference
                    let anchor = slugify(ref_text);
                    format!(
                        "<a class=\"reference internal\" href=\"#{}\">{}</a>",
                        anchor,
                        html_escape::encode_text(ref_text)
                    )
                };

                let placeholder = format!("\x00ROLE{}\x00", role_replacements.len());
                role_replacements.push(html);
                placeholder
            })
            .to_string();

        // Process bare word references: Word_ (without backticks)
        // These are internal references to link targets
        let bare_ref_re = Regex::new(r"\b([A-Za-z][A-Za-z0-9_.]*[A-Za-z0-9])_\b").unwrap();
        let result_with_placeholders = bare_ref_re
            .replace_all(&result_with_placeholders, |caps: &regex::Captures| {
                let ref_text = &caps[1];
                let anchor = slugify(ref_text);
                let html = format!(
                    "<a class=\"reference internal\" href=\"#{}\">{}</a>",
                    anchor,
                    html_escape::encode_text(ref_text)
                );
                let placeholder = format!("\x00ROLE{}\x00", role_replacements.len());
                role_replacements.push(html);
                placeholder
            })
            .to_string();

        // Now HTML escape the result (placeholders will be preserved since they don't contain special chars)
        let mut result = html_escape::encode_text(&result_with_placeholders).to_string();

        // Process inline code with placeholders to protect content from bold/italic processing
        // Double backticks: ``code``
        let code_re = Regex::new(r"``([^`]+)``").unwrap();
        result = code_re
            .replace_all(&result, |caps: &regex::Captures| {
                let code_content = &caps[1];
                let html = format!("<code>{}</code>", code_content);
                let placeholder = format!("\x00ROLE{}\x00", role_replacements.len());
                role_replacements.push(html);
                placeholder
            })
            .to_string();

        // Single backtick inline code: `code`
        // References (`text`_) were already processed and replaced with placeholders,
        // so we can safely match remaining single backticks
        let single_code_re = Regex::new(r"`([^`]+)`").unwrap();
        result = single_code_re
            .replace_all(&result, |caps: &regex::Captures| {
                let code_content = &caps[1];
                let html = format!(
                    "<code class=\"code docutils literal notranslate\"><span class=\"pre\">{}</span></code>",
                    code_content
                );
                let placeholder = format!("\x00ROLE{}\x00", role_replacements.len());
                role_replacements.push(html);
                placeholder
            })
            .to_string();

        // Process bold: **text** (must be done before italic)
        let bold_re = Regex::new(r"\*\*([^*]+)\*\*").unwrap();
        result = bold_re
            .replace_all(&result, "<strong>$1</strong>")
            .to_string();

        // Process italic: *text* (after bold replacement, so ** is already gone)
        let italic_re = Regex::new(r"\*([^*]+)\*").unwrap();
        result = italic_re.replace_all(&result, "<em>$1</em>").to_string();

        // Restore all HTML from placeholders (roles and code)
        for (i, html) in role_replacements.iter().enumerate() {
            let placeholder = format!("\x00ROLE{}\x00", i);
            result = result.replace(&placeholder, html);
        }

        result
    }

    /// Render Markdown content to HTML.
    pub fn render_markdown(&self, content: &MarkdownContent) -> String {
        let mut html = String::new();

        for node in &content.ast {
            html.push_str(&self.render_markdown_node(node));
            html.push('\n');
        }

        html
    }

    /// Render a single Markdown node to HTML.
    fn render_markdown_node(&self, node: &MarkdownNode) -> String {
        match node {
            MarkdownNode::Heading { text, level, .. } => {
                let slug = slugify(text);
                let level = (*level).min(6).max(1);
                format!(
                    "<h{level} id=\"{slug}\">{text}</h{level}>",
                    level = level,
                    slug = slug,
                    text = html_escape::encode_text(text)
                )
            }

            MarkdownNode::Paragraph { content, .. } => {
                let rendered = self.render_markdown_inline(content);
                format!("<p>{}</p>", rendered)
            }

            MarkdownNode::CodeBlock {
                language, content, ..
            } => self.highlight_code(content, language.as_deref()),

            MarkdownNode::List {
                items,
                ordered,
                ..
            } => {
                let tag = if *ordered { "ol" } else { "ul" };
                let items_html: String = items
                    .iter()
                    .map(|item| format!("<li>{}</li>", self.render_markdown_inline(item)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("<{}>\n{}\n</{}>", tag, items_html, tag)
            }

            MarkdownNode::Table { headers, rows, .. } => {
                let mut html = String::from("<table>\n");

                if !headers.is_empty() {
                    html.push_str("<thead>\n<tr>\n");
                    for header in headers {
                        html.push_str(&format!(
                            "<th>{}</th>\n",
                            html_escape::encode_text(header)
                        ));
                    }
                    html.push_str("</tr>\n</thead>\n");
                }

                if !rows.is_empty() {
                    html.push_str("<tbody>\n");
                    for row in rows {
                        html.push_str("<tr>\n");
                        for cell in row {
                            html.push_str(&format!(
                                "<td>{}</td>\n",
                                html_escape::encode_text(cell)
                            ));
                        }
                        html.push_str("</tr>\n");
                    }
                    html.push_str("</tbody>\n");
                }

                html.push_str("</table>");
                html
            }
        }
    }

    /// Render inline Markdown markup (bold, italic, code, links).
    fn render_markdown_inline(&self, text: &str) -> String {
        let mut result = html_escape::encode_text(text).to_string();

        // Process inline code: `code`
        let code_re = Regex::new(r"`([^`]+)`").unwrap();
        result = code_re
            .replace_all(&result, "<code>$1</code>")
            .to_string();

        // Process bold: **text** or __text__ (must be done before italic)
        let bold_star_re = Regex::new(r"\*\*([^*]+)\*\*").unwrap();
        result = bold_star_re
            .replace_all(&result, "<strong>$1</strong>")
            .to_string();
        let bold_under_re = Regex::new(r"__([^_]+)__").unwrap();
        result = bold_under_re
            .replace_all(&result, "<strong>$1</strong>")
            .to_string();

        // Process italic: *text* or _text_ (after bold replacement)
        let italic_star_re = Regex::new(r"\*([^*]+)\*").unwrap();
        result = italic_star_re
            .replace_all(&result, "<em>$1</em>")
            .to_string();
        let italic_under_re = Regex::new(r"_([^_]+)_").unwrap();
        result = italic_under_re
            .replace_all(&result, "<em>$1</em>")
            .to_string();

        // Process links: [text](url)
        let link_re = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap();
        result = link_re
            .replace_all(&result, |caps: &regex::Captures| {
                let text = &caps[1];
                let url = &caps[2];
                format!("<a href=\"{}\">{}</a>", html_escape::encode_text(url), text)
            })
            .to_string();

        result
    }
}

/// Extract plain text from RST markup for use in slugs.
/// Strips inline code backticks, roles like :ref: and :doc:, etc.
pub fn extract_plain_text_for_slug(text: &str) -> String {
    let mut result = text.to_string();

    // Remove RST roles like :ref:`text <target>` -> text
    // Match :role:`display text <target>` or :role:`target`
    // Use a non-greedy match and trim the display text
    let role_re = regex::Regex::new(r":(\w+):`([^`<]+?)(?:\s*<[^>]+>)?`").unwrap();
    result = role_re
        .replace_all(&result, |caps: &regex::Captures| caps[2].trim().to_string())
        .to_string();

    // Remove inline code backticks: `text` -> text
    let code_re = regex::Regex::new(r"`([^`]+)`").unwrap();
    result = code_re.replace_all(&result, "$1").to_string();

    // Remove any remaining backticks
    result = result.replace('`', "");

    result
}

/// Convert text to a URL-safe slug for anchor IDs.
pub fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c
            } else if c.is_whitespace() || c == '-' || c == '_' || c == '.' {
                // Treat periods as word separators (e.g., "Action.button" -> "action-button")
                '-'
            } else {
                '\0'
            }
        })
        .filter(|c| *c != '\0')
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("Introduction"), "introduction");
        assert_eq!(slugify("API Reference"), "api-reference");
        assert_eq!(slugify("foo_bar-baz"), "foo-bar-baz");
        // Periods should become hyphens for class.method style names
        assert_eq!(slugify("Action.button"), "action-button");
        assert_eq!(slugify("Action.delete"), "action-delete");
    }

    #[test]
    fn test_extract_plain_text_for_slug() {
        // Role with display text and target
        assert_eq!(
            extract_plain_text_for_slug("`after` (:ref:`evaluated <evaluate>`)"),
            "after (evaluated)"
        );
        // Just inline code
        assert_eq!(extract_plain_text_for_slug("`display_name`"), "display_name");
        // Multiple elements
        assert_eq!(
            extract_plain_text_for_slug("`foo` and :doc:`Bar`"),
            "foo and Bar"
        );
    }

    #[test]
    fn test_render_rst_title() {
        let renderer = HtmlRenderer::new();
        let node = RstNode::Title {
            text: "Introduction".to_string(),
            level: 1,
            line: 1,
        };
        let html = renderer.render_rst_node(&node);
        // Note: id is now on the parent <section> tag, not the heading itself
        assert_eq!(html, "<h1>Introduction<a class=\"headerlink\" href=\"#introduction\" title=\"Link to this heading\">¶</a></h1>");
    }

    #[test]
    fn test_render_rst_paragraph() {
        let renderer = HtmlRenderer::new();
        let node = RstNode::Paragraph {
            content: "This is a **bold** statement.".to_string(),
            line: 1,
        };
        let html = renderer.render_rst_node(&node);
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.starts_with("<p>"));
        assert!(html.ends_with("</p>"));
    }

    #[test]
    fn test_render_rst_code_block() {
        let renderer = HtmlRenderer::new();
        let node = RstNode::CodeBlock {
            language: Some("python".to_string()),
            content: "print('hello')".to_string(),
            line: 1,
        };
        let html = renderer.render_rst_node(&node);
        // Syntect generates <pre style="..."> with inline styles
        assert!(html.contains("<pre"), "should have pre tag");
        // Code content should be present (possibly with span styling)
        assert!(html.contains("print"), "should contain 'print'");
        assert!(html.contains("hello"), "should contain 'hello'");
        // Syntect uses inline styles for syntax highlighting
        assert!(html.contains("style="), "should have inline styles for highlighting");
    }

    #[test]
    fn test_python_syntax_highlighting() {
        let renderer = HtmlRenderer::new();

        // Test with Python code that has multiple syntactic elements
        let python_code = r#"def greet(name):
    """A docstring."""
    if name:
        print(f"Hello, {name}!")
    return True"#;

        let node = RstNode::CodeBlock {
            language: Some("python".to_string()),
            content: python_code.to_string(),
            line: 1,
        };
        let html = renderer.render_rst_node(&node);

        // Verify syntect found Python syntax (not plain text)
        // Python keywords like 'def', 'if', 'return' should be in colored spans
        assert!(html.contains("<span"), "should have span elements for syntax highlighting");

        // Count the number of styled spans - Python code should have many
        let span_count = html.matches("<span style=").count();
        assert!(span_count >= 5, "Python code should have multiple highlighted spans, got {}", span_count);

        // Verify different colors are used (different syntax elements get different colors)
        // Extract all color values from style attributes
        let colors: Vec<&str> = html.match_indices("color:#")
            .map(|(i, _)| &html[i+7..i+13])
            .collect();
        let unique_colors: std::collections::HashSet<_> = colors.iter().collect();
        assert!(unique_colors.len() >= 2, "should have at least 2 different colors for syntax highlighting, got {:?}", unique_colors);

        // Verify the code content is present
        assert!(html.contains("greet"), "should contain function name");
        assert!(html.contains("docstring"), "should contain docstring text");
        assert!(html.contains("Hello"), "should contain string content");
    }

    #[test]
    fn test_render_rst_list() {
        let renderer = HtmlRenderer::new();
        let node = RstNode::List {
            items: vec!["Item 1".to_string(), "Item 2".to_string()],
            ordered: false,
            line: 1,
        };
        let html = renderer.render_rst_node(&node);
        assert!(html.starts_with("<ul class=\"simple\">"));
        assert!(html.contains("<li>Item 1</li>"));
        assert!(html.contains("<li>Item 2</li>"));
        assert!(html.ends_with("</ul>"));
    }

    #[test]
    fn test_render_inline_markup() {
        let renderer = HtmlRenderer::new();

        // Bold
        let result = renderer.render_rst_inline("This is **bold** text.");
        assert!(result.contains("<strong>bold</strong>"));

        // Italic
        let result = renderer.render_rst_inline("This is *italic* text.");
        assert!(result.contains("<em>italic</em>"));

        // Code (double backticks)
        let result = renderer.render_rst_inline("This is ``code`` text.");
        assert!(result.contains("<code>code</code>"));
    }

    #[test]
    fn test_rst_single_backtick_inline_code() {
        let renderer = HtmlRenderer::new();

        // Single backticks should render as <code class="code docutils literal notranslate"><span class="pre">
        let result = renderer.render_rst_inline("Use `my_function()` to call it.");
        assert!(
            result.contains("<code class=\"code docutils literal notranslate\"><span class=\"pre\">my_function()</span></code>"),
            "single backticks should render as code.docutils, got: {}",
            result
        );
        assert!(!result.contains("`my_function()`"), "backticks should not appear in output");
    }

    #[test]
    fn test_rst_external_link() {
        let renderer = HtmlRenderer::new();

        // External link with URL
        let result = renderer.render_rst_inline(
            "See the `howto <https://docs.iommi.rocks/cookbook.html>`_ for examples."
        );
        assert!(
            result.contains("<a class=\"reference external\" href=\"https://docs.iommi.rocks/cookbook.html\">howto</a>"),
            "external link should render correctly with class, got: {}",
            result
        );
        assert!(
            !result.contains("https://docs.iommi.rocks/cookbook.html\">https"),
            "URL should not be visible in link text"
        );
    }

    #[test]
    fn test_rst_external_link_with_complex_url() {
        let renderer = HtmlRenderer::new();

        // External link with fragment
        let result = renderer.render_rst_inline(
            "`howto <https://docs.iommi.rocks//cookbook_parts_pages.html#parts-pages>`_"
        );
        assert!(
            result.contains("href=\"https://docs.iommi.rocks//cookbook_parts_pages.html#parts-pages\""),
            "URL with fragment should be preserved, got: {}",
            result
        );
        assert!(
            result.contains("class=\"reference external\""),
            "external link should have reference external class, got: {}",
            result
        );
        assert!(
            result.contains(">howto</a>"),
            "display text should be 'howto', got: {}",
            result
        );
    }

    #[test]
    fn test_rst_internal_reference() {
        let renderer = HtmlRenderer::new();

        // Internal reference (no URL)
        let result = renderer.render_rst_inline("See `my-section`_ for details.");
        assert!(
            result.contains("<a class=\"reference internal\" href=\"#my-section\">my-section</a>"),
            "internal reference should create anchor link with class, got: {}",
            result
        );
    }

    #[test]
    fn test_full_rst_document_with_code_block() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Test Document
=============

Here is some code:

.. code-block:: python

   class Bar(models.Model):
       b = models.ForeignKey(Foo, on_delete=models.CASCADE)
       c = models.CharField(max_length=255)

Now I can display a list of Bar in a table."#;

        // Create a temporary file for the parser
        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Should have proper section and heading (= is first underline char, so level 1)
        // The id is now on the section, not the heading
        assert!(html.contains("<section id=\"test-document\">"));
        assert!(html.contains("<h1>Test Document<a class=\"headerlink\" href=\"#test-document\" title=\"Link to this heading\">¶</a></h1>"));

        // Should have code block with pre tag (syntect generates <pre style=...>)
        assert!(html.contains("<pre"), "should have pre tag");
        assert!(html.contains("Bar"), "should contain code content");
        assert!(!html.contains("<p>.. code-block::"), "Directive should not appear as paragraph");
        assert!(!html.contains("<p>class Bar"), "Code should not be in paragraph tags");

        // Should have the final paragraph
        assert!(html.contains("Now I can display"));
    }

    #[test]
    fn test_code_block_directive_python_syntax_highlighting() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Code Example
============

.. code-block:: python

   def greet(name):
       """Say hello."""
       if name:
           print(f"Hello, {name}!")
       return True
"#;

        // Create a temporary file for the parser
        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Verify syntax highlighting is applied
        assert!(html.contains("<pre style="), "should have pre tag with inline styles");
        assert!(html.contains("<span style="), "should have span elements with syntax colors");

        // Count styled spans - Python code should have multiple highlighted elements
        let span_count = html.matches("<span style=").count();
        assert!(span_count >= 5, "Python code should have multiple highlighted spans, got {}", span_count);

        // Verify different colors are used for different syntax elements
        let colors: Vec<&str> = html.match_indices("color:#")
            .map(|(i, _)| &html[i+7..i+13])
            .collect();
        let unique_colors: std::collections::HashSet<_> = colors.iter().collect();
        assert!(unique_colors.len() >= 2, "should have multiple colors for syntax highlighting, got {:?}", unique_colors);

        // Verify code content is present
        assert!(html.contains("greet"), "should contain function name");
        assert!(html.contains("hello"), "should contain docstring text");
        assert!(html.contains("Hello"), "should contain string content");

        // Verify it's wrapped in highlight div
        assert!(html.contains("highlight-python"), "should have highlight-python wrapper");
    }

    #[test]
    fn test_toctree_directive_with_explicit_titles() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        // Test with explicit titles using "Title <path>" syntax
        let content = r#"Welcome
=======

.. toctree::
   :maxdepth: 2
   :caption: Contents

   Introduction <intro>
   Tutorial Guide <tutorial/index>
   API Reference <api/reference>
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Toctree should be recognized as a directive, not rendered as paragraph
        assert!(!html.contains("<p>.. toctree::"), "toctree should not appear as paragraph");
        assert!(!html.contains("<p>:maxdepth:"), "toctree options should not appear as paragraph");

        // Should have toctree wrapper
        assert!(html.contains("toctree-wrapper"), "should have toctree-wrapper class");

        // Should have caption
        assert!(html.contains("Contents"), "should have caption text");

        // Should have links to documents with correct hrefs
        assert!(html.contains("intro.html"), "should have link to intro");
        assert!(html.contains("tutorial/index.html"), "should have link to tutorial/index");
        assert!(html.contains("api/reference.html"), "should have link to api/reference");

        // Should display explicit titles, NOT filenames
        assert!(html.contains(">Introduction<"), "should show 'Introduction' as link text");
        assert!(html.contains(">Tutorial Guide<"), "should show 'Tutorial Guide' as link text");
        assert!(html.contains(">API Reference<"), "should show 'API Reference' as link text");

        // Should NOT show just the filename
        assert!(!html.contains(">intro<"), "should not show just 'intro' as link text");
        assert!(!html.contains(">index<"), "should not show just 'index' as link text");
        assert!(!html.contains(">reference<"), "should not show just 'reference' as link text");
    }

    #[test]
    fn test_toctree_with_document_titles_from_registry() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        // Without explicit titles, should look up titles from document registry
        let content = r#"Index
=====

.. toctree::

   intro
   tutorial/getting-started
   unknown-doc
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        // Create renderer with document titles registered
        let mut renderer = HtmlRenderer::new();
        renderer.register_document_title("intro", "Introduction to the Project");
        renderer.register_document_title("tutorial/getting-started", "Getting Started Guide");
        // Note: unknown-doc is NOT registered

        let html = renderer.render_document_content(&doc.content);

        // Should use titles from the registry
        assert!(html.contains(">Introduction to the Project<"), "should show registered title for intro");
        assert!(html.contains(">Getting Started Guide<"), "should show registered title for tutorial");

        // Unknown docs should fall back to path
        assert!(html.contains(">unknown-doc<"), "should fall back to path for unknown docs");

        // Should still have correct hrefs
        assert!(html.contains("intro.html"), "should have correct href for intro");
        assert!(html.contains("tutorial/getting-started.html"), "should have correct href for tutorial");
    }

    #[test]
    fn test_toctree_explicit_title_overrides_registry() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        // Explicit titles should override registry titles
        let content = r#"Index
=====

.. toctree::

   Custom Title <intro>
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.register_document_title("intro", "Introduction to the Project");

        let html = renderer.render_document_content(&doc.content);

        // Explicit title should win over registry
        assert!(html.contains(">Custom Title<"), "explicit title should override registry");
        assert!(!html.contains(">Introduction to the Project<"), "registry title should not appear");
    }

    #[test]
    fn test_toctree_without_caption() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Index
=====

.. toctree::

   Page One <page1>
   Page Two <page2>
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Should have links but no caption
        assert!(html.contains("page1.html"), "should have link to page1");
        assert!(html.contains("page2.html"), "should have link to page2");
        assert!(html.contains(">Page One<"), "should show 'Page One' as link text");
        assert!(html.contains(">Page Two<"), "should show 'Page Two' as link text");
        assert!(!html.contains("caption"), "should not have caption class when no caption specified");
    }

    #[test]
    fn test_link_target_not_rendered_in_html() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

.. _my-link-target:

Some paragraph after the link target.

.. _another-target:

Another paragraph.
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Link targets should NOT appear as visible text in the output
        assert!(!html.contains(".. _my-link-target"), "link target syntax should not appear");
        assert!(!html.contains(".. _another-target"), "link target syntax should not appear");
        assert!(!html.contains("_my-link-target:"), "link target name should not appear as text");

        // The content should still be there
        assert!(html.contains("Some paragraph after"), "paragraph after link target should be present");
        assert!(html.contains("Another paragraph"), "second paragraph should be present");

        // Link targets should NOT be rendered as paragraphs
        assert!(!html.contains("<p>.. _"), "link target should not be in a paragraph tag");
    }

    #[test]
    fn test_link_target_creates_anchor_for_ref() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

.. _installation-guide:

Installation
------------

Follow these steps to install.

See :ref:`installation-guide` for more info.
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Should have an anchor/id for the link target
        assert!(html.contains("id=\"installation-guide\""), "should have anchor id for link target");

        // The :ref: role should create a link in "target.html#target" format
        assert!(html.contains("href=\"installation-guide.html#installation-guide\""), "ref should link to the anchor");
    }

    #[test]
    fn test_unknown_rst_construct_not_in_output() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

Some text before.

.. something-unknown something something

More text after.

.. another-thing with arguments

Final paragraph.
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Unknown RST constructs should NOT appear in output
        assert!(!html.contains("something-unknown"), "unknown construct should not appear");
        assert!(!html.contains("another-thing"), "unknown construct should not appear");
        assert!(!html.contains(".. "), "RST syntax should not appear in output");

        // Regular content should still be there
        assert!(html.contains("Some text before"), "text before should be present");
        assert!(html.contains("More text after"), "text after should be present");
        assert!(html.contains("Final paragraph"), "final paragraph should be present");
    }

    #[test]
    fn test_unknown_directive_produces_no_visible_output() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

Some text before.

.. unknown-directive:: argument
   :option: value

   Some content inside the directive.

Some text after.
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // The directive should not appear as visible content
        assert!(!html.contains("unknown-directive"), "directive name should not appear in output");
        assert!(!html.contains(":option:"), "directive options should not appear in output");

        // The surrounding content should still be there
        assert!(html.contains("Some text before"), "text before directive should be present");
        assert!(html.contains("Some text after"), "text after directive should be present");

        // Should not have any <p> tags containing directive syntax
        assert!(!html.contains("<p>.."), "directive should not be rendered as paragraph");
    }

    #[test]
    fn test_toctree_hidden() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Index
=====

.. toctree::
   :hidden:

   secret_page
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Hidden toctree should have hidden class or style
        assert!(html.contains("toctree-wrapper"), "should still have wrapper");
        assert!(
            html.contains("hidden") || html.contains("display: none") || html.contains("display:none"),
            "hidden toctree should be hidden"
        );
    }

    #[test]
    fn test_raw_html_directive_inserts_html() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

Some text before.

.. raw:: html

   <div class="custom-widget">
     <span id="special">Custom HTML content</span>
   </div>

Some text after.
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // The raw HTML should be inserted directly without escaping
        assert!(
            html.contains("<div class=\"custom-widget\">"),
            "raw HTML div should be present"
        );
        assert!(
            html.contains("<span id=\"special\">Custom HTML content</span>"),
            "raw HTML span should be present"
        );

        // Surrounding content should still be there
        assert!(html.contains("Some text before"), "text before should be present");
        assert!(html.contains("Some text after"), "text after should be present");

        // The directive syntax should NOT appear in the output
        assert!(!html.contains(".. raw::"), "directive syntax should not appear");
    }

    #[test]
    fn test_ref_role_with_explicit_title() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

See :ref:`attrs <attributes>`.
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // The link should have text "attrs" wrapped in std-ref span, NOT "attrs <attributes>"
        assert!(
            html.contains("<span class=\"std std-ref\">attrs</span></a>"),
            "link text should be 'attrs' in std-ref span, got: {}",
            html
        );

        // The href should point to attributes.html#attributes
        assert!(
            html.contains("href=\"attributes.html#attributes\""),
            "link should point to attributes.html#attributes, got: {}",
            html
        );

        // Should NOT contain the raw angle bracket syntax in visible text
        assert!(
            !html.contains("attrs &lt;attributes&gt;"),
            "should not show escaped angle brackets in text"
        );
        assert!(
            !html.contains("attrs <attributes>"),
            "should not show raw angle brackets in link text"
        );
    }

    #[test]
    fn test_blockquote_rendering() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

Type: `Union[int, str]`

    See :ref:`after <after>`
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Blockquote should be rendered
        assert!(
            html.contains("<blockquote>"),
            "indented text should be wrapped in blockquote, got: {}",
            html
        );

        // The :ref: role inside blockquote should link to after.html#after
        assert!(
            html.contains("href=\"after.html#after\""),
            "ref should link to after.html#after, got: {}",
            html
        );

        // The link text should be "after" wrapped in std-ref span
        assert!(
            html.contains("<span class=\"std std-ref\">after</span></a>"),
            "link text should be 'after' in std-ref span, got: {}",
            html
        );
    }

    #[test]
    fn test_complex_rst_with_blockquote_and_ref() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"`after`       (:ref:`evaluated <evaluate>`)
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

Type: `Union[int, str]`

    See :ref:`after <after>`
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Title should be recognized
        assert!(
            doc.title.contains("after"),
            "title should contain 'after', got: {}",
            doc.title
        );

        // Blockquote should be rendered
        assert!(
            html.contains("<blockquote>"),
            "indented text should be wrapped in blockquote, got: {}",
            html
        );

        // The :ref: in the title should link to evaluate.html#evaluate
        assert!(
            html.contains("href=\"evaluate.html#evaluate\""),
            "ref in title should link to evaluate.html#evaluate, got: {}",
            html
        );

        // The :ref: in the blockquote should link to after.html#after
        assert!(
            html.contains("href=\"after.html#after\""),
            "ref in blockquote should link to after.html#after, got: {}",
            html
        );
    }
}
