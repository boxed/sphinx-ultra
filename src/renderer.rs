//! AST-to-HTML renderer for RST and Markdown documents.

use crate::config::BuildConfig;
use crate::directives::{Directive, DirectiveRegistry};
use crate::document::{DocumentContent, MarkdownContent, MarkdownNode, RstContent, RstNode};
use crate::parser::Parser;
use crate::roles::{Role, RoleRegistry};
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;
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
    /// Source directory for resolving relative paths (e.g., for literalinclude)
    source_dir: Option<PathBuf>,
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
            source_dir: None,
        }
    }

    /// Set the source directory for resolving relative paths in directives like literalinclude.
    pub fn set_source_dir(&mut self, source_dir: PathBuf) {
        self.source_dir = Some(source_dir);
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

                // Handle literalinclude specially since it needs to read files from source_dir
                if name == "literalinclude" {
                    let filename = args.first().map(|s| s.as_str()).unwrap_or("");
                    return self.render_literalinclude(filename, options);
                }

                // Handle include specially since it needs to parse and render RST content
                if name == "include" {
                    let filename = args.first().map(|s| s.as_str()).unwrap_or("");
                    return self.render_include(filename, options);
                }

                // Pre-process content for inline RST markup (roles like :ref:, :doc:, etc.)
                // This is needed for admonitions and other directives that contain RST text
                // Skip processing for directives that should receive raw content (like raw, code-block, literalinclude)
                let raw_content_directives = ["raw", "code-block", "code", "sourcecode", "literalinclude", "highlight"];
                let processed_content: Vec<String> = if raw_content_directives.contains(&name.as_str()) {
                    content.lines().map(String::from).collect()
                } else {
                    content
                        .lines()
                        .map(|line| self.render_rst_inline(line))
                        .collect()
                };

                // Convert to Directive struct for processing
                let directive = Directive {
                    name: name.clone(),
                    arguments: args.clone(),
                    options: options.clone(),
                    content: processed_content,
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

    /// Render a literalinclude directive by reading a file and optionally applying filters.
    fn render_literalinclude(&self, filename: &str, options: &HashMap<String, String>) -> String {
        // Resolve the file path relative to source_dir
        let file_path = if let Some(ref source_dir) = self.source_dir {
            source_dir.join(filename)
        } else {
            PathBuf::from(filename)
        };

        // Read the file content
        let content = match std::fs::read_to_string(&file_path) {
            Ok(content) => content,
            Err(e) => {
                return format!(
                    "<!-- literalinclude error: could not read '{}': {} -->",
                    filename, e
                );
            }
        };

        // Handle :pyobject: option - extract a specific Python object
        let content = if let Some(pyobject) = options.get("pyobject") {
            match self.extract_python_object(&content, pyobject) {
                Some(extracted) => extracted,
                None => {
                    return format!(
                        "<!-- literalinclude error: could not find pyobject '{}' in '{}' -->",
                        pyobject, filename
                    );
                }
            }
        } else {
            content
        };

        // Apply line-based filtering
        let mut lines: Vec<&str> = content.lines().collect();

        // Handle start-after option (find line containing this text and start after it)
        if let Some(start_after) = options.get("start-after") {
            if let Some(pos) = lines.iter().position(|line| line.contains(start_after.as_str())) {
                lines = lines[pos + 1..].to_vec();
            }
        }

        // Handle start-at option (find line containing this text and start at it, inclusive)
        if let Some(start_at) = options.get("start-at") {
            if let Some(pos) = lines.iter().position(|line| line.contains(start_at.as_str())) {
                lines = lines[pos..].to_vec();
            }
        }

        // Handle end-before option (find line containing this text and end before it)
        if let Some(end_before) = options.get("end-before") {
            if let Some(pos) = lines.iter().position(|line| line.contains(end_before.as_str())) {
                lines = lines[..pos].to_vec();
            }
        }

        // Handle start-line option (0-based: skip first N lines, like Sphinx)
        if let Some(start_line) = options.get("start-line") {
            if let Ok(start) = start_line.parse::<usize>() {
                if start <= lines.len() {
                    lines = lines[start..].to_vec();
                }
            }
        }

        // Handle end-line option (1-based indexing, exclusive)
        if let Some(end_line) = options.get("end-line") {
            if let Ok(end) = end_line.parse::<usize>() {
                if end > 0 && end <= lines.len() {
                    lines = lines[..end].to_vec();
                }
            }
        }

        // Handle :lines: option (e.g., "1-10", "1,3,5-7")
        if let Some(lines_spec) = options.get("lines") {
            let selected_lines = self.parse_lines_spec(lines_spec, lines.len());
            lines = selected_lines
                .iter()
                .filter_map(|&i| lines.get(i).copied())
                .collect();
        }

        // Handle dedent option
        if let Some(dedent_str) = options.get("dedent") {
            if let Ok(dedent) = dedent_str.parse::<usize>() {
                lines = lines
                    .iter()
                    .map(|line| {
                        if line.len() >= dedent {
                            &line[dedent.min(line.len() - line.trim_start().len())..]
                        } else {
                            line.trim_start()
                        }
                    })
                    .collect();
            }
        }

        let filtered_content = lines.join("\n");

        // Determine language for syntax highlighting
        let language = options
            .get("language")
            .cloned()
            .or_else(|| {
                std::path::Path::new(filename)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| {
                        match ext {
                            "py" => "python",
                            "rs" => "rust",
                            "js" => "javascript",
                            "ts" => "typescript",
                            "cpp" | "cc" | "cxx" => "cpp",
                            "c" => "c",
                            "h" | "hpp" => "cpp",
                            "java" => "java",
                            "go" => "go",
                            "php" => "php",
                            "rb" => "ruby",
                            "sh" | "bash" => "bash",
                            "ps1" => "powershell",
                            "sql" => "sql",
                            "xml" => "xml",
                            "html" | "htm" => "html",
                            "css" => "css",
                            "json" => "json",
                            "yaml" | "yml" => "yaml",
                            "toml" => "toml",
                            "ini" | "cfg" => "ini",
                            "md" => "markdown",
                            "rst" => "rst",
                            "tex" => "latex",
                            _ => "text",
                        }
                        .to_string()
                    })
            })
            .unwrap_or_else(|| "text".to_string());

        // Apply syntax highlighting
        let theme = &self.theme_set.themes[&self.theme_name];
        let syntax = self
            .syntax_set
            .find_syntax_by_token(&language)
            .or_else(|| self.syntax_set.find_syntax_by_extension(&language))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let highlighted = match highlighted_html_for_string(&filtered_content, &self.syntax_set, syntax, theme) {
            Ok(html) => html,
            Err(_) => {
                let escaped = html_escape::encode_text(&filtered_content);
                format!("<pre><code>{}</code></pre>", escaped)
            }
        };

        // Build the final HTML
        let mut html = String::new();

        // Add caption if present
        if let Some(caption) = options.get("caption") {
            // Replace {filename} placeholder with actual filename
            let caption_text = caption.replace("{filename}", filename);
            html.push_str(&format!(
                "<div class=\"code-block-caption\"><span class=\"caption-text\">{}</span></div>\n",
                html_escape::encode_text(&caption_text)
            ));
        }

        html.push_str(&format!(
            "<div class=\"highlight-{} notranslate\">{}</div>",
            language, highlighted
        ));

        html
    }

    /// Render an include directive by reading a file, optionally filtering lines,
    /// parsing as RST, and rendering to HTML.
    fn render_include(&self, filename: &str, options: &HashMap<String, String>) -> String {
        // Resolve the file path relative to source_dir
        let file_path = if let Some(ref source_dir) = self.source_dir {
            source_dir.join(filename)
        } else {
            PathBuf::from(filename)
        };

        // Read the file content
        let content = match std::fs::read_to_string(&file_path) {
            Ok(content) => content,
            Err(e) => {
                return format!(
                    "<!-- include error: could not read '{}': {} -->",
                    filename, e
                );
            }
        };

        // Apply line-based filtering
        let mut lines: Vec<&str> = content.lines().collect();

        // Handle start-line option (0-based: skip first N lines, like Sphinx)
        if let Some(start_line) = options.get("start-line") {
            if let Ok(start) = start_line.parse::<usize>() {
                if start <= lines.len() {
                    lines = lines[start..].to_vec();
                }
            }
        }

        // Handle end-line option (1-based indexing, exclusive like Sphinx)
        if let Some(end_line) = options.get("end-line") {
            if let Ok(end) = end_line.parse::<usize>() {
                if end > 0 && end <= lines.len() {
                    lines = lines[..end].to_vec();
                }
            }
        }

        // Handle start-after option (find line containing this text and start after it)
        if let Some(start_after) = options.get("start-after") {
            if let Some(pos) = lines.iter().position(|line| line.contains(start_after.as_str())) {
                lines = lines[pos + 1..].to_vec();
            }
        }

        // Handle end-before option (find line containing this text and end before it)
        if let Some(end_before) = options.get("end-before") {
            if let Some(pos) = lines.iter().position(|line| line.contains(end_before.as_str())) {
                lines = lines[..pos].to_vec();
            }
        }

        let filtered_content = lines.join("\n");

        // Parse the content as RST
        let config = BuildConfig::default();
        let parser = match Parser::new(&config) {
            Ok(p) => p,
            Err(e) => {
                return format!(
                    "<!-- include error: could not create parser: {} -->",
                    e
                );
            }
        };

        // Parse the included content - use a dummy path with .rst extension for RST parsing
        let dummy_path = file_path.with_extension("rst");
        let document = match parser.parse(&dummy_path, &filtered_content) {
            Ok(doc) => doc,
            Err(e) => {
                return format!(
                    "<!-- include error: could not parse '{}': {} -->",
                    filename, e
                );
            }
        };

        // Render the parsed content
        self.render_document_content(&document.content)
    }

    /// Parse a lines specification like "1-10", "1,3,5-7", "1-10,15,20-25"
    /// Returns 0-based indices
    fn parse_lines_spec(&self, spec: &str, total_lines: usize) -> Vec<usize> {
        let mut result = Vec::new();

        for part in spec.split(',') {
            let part = part.trim();
            if part.contains('-') {
                // Range like "1-10"
                let parts: Vec<&str> = part.split('-').collect();
                if parts.len() == 2 {
                    if let (Ok(start), Ok(end)) = (parts[0].trim().parse::<usize>(), parts[1].trim().parse::<usize>()) {
                        for i in start..=end {
                            if i > 0 && i <= total_lines {
                                result.push(i - 1); // Convert to 0-based
                            }
                        }
                    }
                }
            } else {
                // Single line number
                if let Ok(line) = part.parse::<usize>() {
                    if line > 0 && line <= total_lines {
                        result.push(line - 1); // Convert to 0-based
                    }
                }
            }
        }

        result
    }

    /// Extract a Python object (function, class, or method) from source code.
    /// Supports formats like "function_name", "ClassName", or "ClassName.method_name"
    fn extract_python_object(&self, content: &str, pyobject: &str) -> Option<String> {
        let lines: Vec<&str> = content.lines().collect();

        // Check if we're looking for a method (Class.method format)
        if let Some(dot_pos) = pyobject.find('.') {
            let class_name = &pyobject[..dot_pos];
            let method_name = &pyobject[dot_pos + 1..];

            // First find the class
            if let Some((class_start, class_end)) = self.find_python_object_range(&lines, class_name, 0) {
                // Then find the method within the class
                let class_lines: Vec<&str> = lines[class_start..class_end].to_vec();
                if let Some((method_start, method_end)) = self.find_python_object_range(&class_lines, method_name, 1) {
                    return Some(class_lines[method_start..method_end].join("\n"));
                }
            }
            return None;
        }

        // Looking for a top-level function or class
        if let Some((start, end)) = self.find_python_object_range(&lines, pyobject, 0) {
            return Some(lines[start..end].join("\n"));
        }

        None
    }

    /// Find the line range (start, end) of a Python object definition.
    /// `min_indent` is the minimum indentation level to look for (0 for top-level, 1 for methods inside a class)
    fn find_python_object_range(&self, lines: &[&str], name: &str, min_indent: usize) -> Option<(usize, usize)> {
        let def_pattern = format!("def {}(", name);
        let class_pattern = format!("class {}:", name);
        let class_pattern_paren = format!("class {}(", name);

        let mut start_line = None;
        let mut start_indent = 0;

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            let indent_level = indent / 4; // Assuming 4-space indentation (also handle tabs below)

            // Check if this line defines the object we're looking for
            if trimmed.starts_with(&def_pattern)
                || trimmed.starts_with(&class_pattern)
                || trimmed.starts_with(&class_pattern_paren)
            {
                // Check if indentation level matches what we're looking for
                if indent_level >= min_indent {
                    start_line = Some(i);
                    start_indent = indent;
                    break;
                }
            }
        }

        let start = start_line?;

        // Find where this object ends (next line at same or lower indentation that's not empty/comment)
        let mut end = lines.len();
        for (i, line) in lines.iter().enumerate().skip(start + 1) {
            let trimmed = line.trim();

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let indent = line.len() - line.trim_start().len();

            // If we hit a line with same or less indentation, we've exited the object
            // But we need to handle decorators - they start at same indent as def/class
            if indent <= start_indent {
                // Check if it's a decorator for the same object (shouldn't happen after start)
                // or if it's a new definition/statement
                let is_decorator = trimmed.starts_with('@');
                if !is_decorator {
                    end = i;
                    break;
                }
            }
        }

        // Trim trailing empty lines
        while end > start + 1 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }

        Some((start, end))
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

    #[test]
    fn test_ref_role_in_note_directive() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use std::io::Write;

        let content = r#"Title
=====

.. note::

    This tutorial is intended for a reader that is well versed in the Django basics of the ORM,
    urls routing, function based views, and templates.

    It is also expected that you have already installed iommi in your project. Read section 1 of :ref:`Getting started <getting-started>`.
"#;

        let mut temp_file = tempfile::NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(temp_file.path(), content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // The note directive should be rendered as an admonition
        assert!(
            html.contains("admonition note"),
            "note directive should be rendered as admonition, got: {}",
            html
        );

        // The :ref: should link to getting-started.html#getting-started
        assert!(
            html.contains("href=\"getting-started.html#getting-started\""),
            "ref should link to getting-started.html#getting-started, got: {}",
            html
        );

        // The link text should be "Getting started" wrapped in std-ref span
        assert!(
            html.contains("<span class=\"std std-ref\">Getting started</span></a>"),
            "link text should be 'Getting started' in std-ref span, got: {}",
            html
        );
    }

    #[test]
    fn test_literalinclude_basic() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Create a temp directory with a source file to include
        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(&source_file, "def hello():\n    print('Hello, World!')\n").unwrap();

        // Create an RST file that includes the source file
        let rst_content = r#"Title
=====

.. literalinclude:: example.py
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should include the file content with syntax highlighting
        assert!(
            html.contains("highlight-python"),
            "should have python highlighting class, got: {}",
            html
        );
        assert!(
            html.contains("hello") || html.contains("Hello"),
            "should contain the function name, got: {}",
            html
        );
    }

    #[test]
    fn test_literalinclude_with_lines() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Create a temp directory with a source file to include
        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            "# Line 1\n# Line 2\n# Line 3\n# Line 4\n# Line 5\n",
        )
        .unwrap();

        // Create an RST file that includes only lines 2-4
        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :lines: 2-4
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should contain lines 2, 3, 4 but NOT line 1 or 5
        assert!(html.contains("Line 2"), "should contain Line 2, got: {}", html);
        assert!(html.contains("Line 3"), "should contain Line 3, got: {}", html);
        assert!(html.contains("Line 4"), "should contain Line 4, got: {}", html);
        assert!(!html.contains("Line 1"), "should NOT contain Line 1, got: {}", html);
        assert!(!html.contains("Line 5"), "should NOT contain Line 5, got: {}", html);
    }

    #[test]
    fn test_literalinclude_with_start_after_end_before() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Create a temp directory with a source file to include
        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            "# HEADER\ndef main():\n    # START\n    print('included')\n    # END\n    pass\n",
        )
        .unwrap();

        // Create an RST file that includes only content between markers
        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :start-after: # START
   :end-before: # END
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should contain the print line but NOT the markers or other content
        assert!(html.contains("included"), "should contain 'included', got: {}", html);
        assert!(!html.contains("HEADER"), "should NOT contain HEADER, got: {}", html);
        assert!(!html.contains("# START"), "should NOT contain # START marker, got: {}", html);
        assert!(!html.contains("# END"), "should NOT contain # END marker, got: {}", html);
    }

    #[test]
    fn test_literalinclude_with_start_at() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Create a temp directory with a source file to include
        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            "# HEADER\ndef main():\n    # START MARKER\n    print('included')\n    pass\n",
        )
        .unwrap();

        // Create an RST file that includes starting AT the marker (inclusive)
        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :start-at: # START MARKER
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // start-at INCLUDES the matching line (unlike start-after which excludes it)
        assert!(html.contains("# START MARKER"), "should contain '# START MARKER', got: {}", html);
        assert!(html.contains("included"), "should contain 'included', got: {}", html);
        assert!(!html.contains("HEADER"), "should NOT contain HEADER, got: {}", html);
        assert!(!html.contains("def main"), "should NOT contain 'def main', got: {}", html);
    }

    #[test]
    fn test_literalinclude_with_caption() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Create a temp directory with a source file to include
        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(&source_file, "print('hello')\n").unwrap();

        // Create an RST file with a caption
        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :caption: My Example Code
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should have a caption
        assert!(
            html.contains("code-block-caption"),
            "should have caption class, got: {}",
            html
        );
        assert!(
            html.contains("My Example Code"),
            "should contain caption text, got: {}",
            html
        );
    }

    #[test]
    fn test_literalinclude_file_not_found() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();

        // Create an RST file that references a non-existent file
        let rst_content = r#"Title
=====

.. literalinclude:: nonexistent.py
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should have an error comment
        assert!(
            html.contains("literalinclude error"),
            "should have error message, got: {}",
            html
        );
    }

    #[test]
    fn test_literalinclude_pyobject_function() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            r#"# Header comment

def first_function():
    """First function docstring."""
    return 1

def target_function():
    """Target function docstring."""
    x = 1
    y = 2
    return x + y

def another_function():
    """Another function."""
    pass
"#,
        )
        .unwrap();

        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :pyobject: target_function
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should contain target_function content
        assert!(html.contains("target_function"), "should contain target_function, got: {}", html);
        assert!(html.contains("Target function docstring"), "should contain docstring, got: {}", html);
        assert!(html.contains("x + y") || html.contains("return"), "should contain function body, got: {}", html);

        // Should NOT contain other functions
        assert!(!html.contains("first_function"), "should NOT contain first_function, got: {}", html);
        assert!(!html.contains("another_function"), "should NOT contain another_function, got: {}", html);
        assert!(!html.contains("Header comment"), "should NOT contain header comment, got: {}", html);
    }

    #[test]
    fn test_literalinclude_pyobject_class() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            r#"def standalone():
    pass

class MyClass:
    """A sample class."""

    def __init__(self):
        self.value = 42

    def method(self):
        return self.value

class OtherClass:
    pass
"#,
        )
        .unwrap();

        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :pyobject: MyClass
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should contain MyClass content
        assert!(html.contains("MyClass"), "should contain MyClass, got: {}", html);
        assert!(html.contains("sample class"), "should contain class docstring, got: {}", html);
        assert!(html.contains("__init__"), "should contain __init__ method, got: {}", html);
        // Note: "self.value" gets split by syntax highlighting spans, so check for "value" instead
        assert!(html.contains("value"), "should contain method body, got: {}", html);

        // Should NOT contain other classes or functions
        assert!(!html.contains("standalone"), "should NOT contain standalone function, got: {}", html);
        assert!(!html.contains("OtherClass"), "should NOT contain OtherClass, got: {}", html);
    }

    #[test]
    fn test_literalinclude_pyobject_method() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            r#"class MyClass:
    def __init__(self):
        self.value = 42

    def target_method(self):
        """The target method."""
        return self.value * 2

    def other_method(self):
        pass
"#,
        )
        .unwrap();

        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :pyobject: MyClass.target_method
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should contain target_method content
        assert!(html.contains("target_method"), "should contain target_method, got: {}", html);
        assert!(html.contains("target method"), "should contain method docstring, got: {}", html);

        // Should NOT contain other methods
        assert!(!html.contains("__init__"), "should NOT contain __init__, got: {}", html);
        assert!(!html.contains("other_method"), "should NOT contain other_method, got: {}", html);
    }

    #[test]
    fn test_literalinclude_pyobject_excludes_imports_and_other_objects() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            r#"#!/usr/bin/env python
"""Module docstring."""

import os
import sys
from pathlib import Path
from typing import Optional, List

# Module-level constant
CONSTANT_VALUE = 42
OTHER_CONSTANT = "hello"

def before_function():
    """A function before the target."""
    return "before"

class BeforeClass:
    """A class before the target."""
    pass

def target_function(arg1, arg2):
    """The target function we want to extract."""
    result = arg1 + arg2
    return result

def after_function():
    """A function after the target."""
    return "after"

class AfterClass:
    """A class after the target."""
    def method(self):
        pass

# Trailing comment
"#,
        )
        .unwrap();

        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :pyobject: target_function
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should contain ONLY target_function content
        assert!(html.contains("target_function"), "should contain target_function, got: {}", html);
        assert!(html.contains("target function we want"), "should contain docstring, got: {}", html);
        assert!(html.contains("arg1") && html.contains("arg2"), "should contain function args, got: {}", html);

        // Should NOT contain imports
        assert!(!html.contains("import os"), "should NOT contain 'import os', got: {}", html);
        assert!(!html.contains("import sys"), "should NOT contain 'import sys', got: {}", html);
        assert!(!html.contains("from pathlib"), "should NOT contain 'from pathlib', got: {}", html);
        assert!(!html.contains("from typing"), "should NOT contain 'from typing', got: {}", html);

        // Should NOT contain module docstring or shebang
        assert!(!html.contains("#!/usr/bin"), "should NOT contain shebang, got: {}", html);
        assert!(!html.contains("Module docstring"), "should NOT contain module docstring, got: {}", html);

        // Should NOT contain constants
        assert!(!html.contains("CONSTANT_VALUE"), "should NOT contain CONSTANT_VALUE, got: {}", html);
        assert!(!html.contains("OTHER_CONSTANT"), "should NOT contain OTHER_CONSTANT, got: {}", html);

        // Should NOT contain other functions
        assert!(!html.contains("before_function"), "should NOT contain before_function, got: {}", html);
        assert!(!html.contains("after_function"), "should NOT contain after_function, got: {}", html);

        // Should NOT contain other classes
        assert!(!html.contains("BeforeClass"), "should NOT contain BeforeClass, got: {}", html);
        assert!(!html.contains("AfterClass"), "should NOT contain AfterClass, got: {}", html);

        // Should NOT contain trailing comment
        assert!(!html.contains("Trailing comment"), "should NOT contain trailing comment, got: {}", html);
    }

    #[test]
    fn test_literalinclude_pyobject_with_end_before() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let source_file = temp_dir.path().join("example.py");
        std::fs::write(
            &source_file,
            r#"import os

def my_function():
    """My function docstring."""
    # First part
    x = 1
    y = 2
    # END MARKER
    # Second part
    z = 3
    return x + y + z

def other_function():
    pass
"#,
        )
        .unwrap();

        let rst_content = r#"Title
=====

.. literalinclude:: example.py
   :pyobject: my_function
   :end-before: # END MARKER
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let parser = Parser::new(&config).unwrap();
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        // Verify options were parsed correctly
        if let crate::document::DocumentContent::RestructuredText(rst) = &doc.content {
            for node in &rst.ast {
                if let crate::document::RstNode::Directive { name, options, .. } = node {
                    if name == "literalinclude" {
                        assert!(options.contains_key("pyobject"), "options should contain 'pyobject': {:?}", options);
                        assert!(options.contains_key("end-before"), "options should contain 'end-before': {:?}", options);
                        assert_eq!(options.get("pyobject").unwrap(), "my_function");
                        assert_eq!(options.get("end-before").unwrap(), "# END MARKER");
                    }
                }
            }
        }

        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(temp_dir.path().to_path_buf());
        let html = renderer.render_document_content(&doc.content);

        // Should contain the function definition and first part
        assert!(html.contains("my_function"), "should contain my_function, got: {}", html);
        assert!(html.contains("First part"), "should contain 'First part', got: {}", html);

        // Should NOT contain content after END MARKER
        assert!(!html.contains("Second part"), "should NOT contain 'Second part', got: {}", html);
        assert!(!html.contains("z = 3"), "should NOT contain 'z = 3', got: {}", html);

        // Should NOT contain the marker itself
        assert!(!html.contains("END MARKER"), "should NOT contain 'END MARKER', got: {}", html);

        // Should NOT contain imports or other functions
        assert!(!html.contains("import os"), "should NOT contain 'import os', got: {}", html);
        assert!(!html.contains("other_function"), "should NOT contain 'other_function', got: {}", html);
    }

    #[test]
    fn test_include_basic() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Create a temp directory with an RST file to include
        let temp_dir = TempDir::new().unwrap();
        let include_file = temp_dir.path().join("included.rst");
        std::fs::write(&include_file, "This is **included** content.\n\nAnother paragraph.\n").unwrap();

        // Create an RST file that includes the other file
        let rst_content = r#"Title
=====

.. include:: included.rst
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let mut parser = Parser::new(&config).unwrap();
        parser.set_source_dir(temp_dir.path().to_path_buf());
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // Should include the content from the included file, rendered as RST
        assert!(
            html.contains("included"),
            "should contain 'included', got: {}",
            html
        );
        assert!(
            html.contains("<strong>included</strong>") || html.contains("<b>included</b>"),
            "should have bold 'included' text, got: {}",
            html
        );
        assert!(
            html.contains("Another paragraph"),
            "should contain 'Another paragraph', got: {}",
            html
        );
    }

    #[test]
    fn test_include_with_start_line() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Create a temp directory with an RST file to include
        // start-line: N means skip the first N lines (0-based, like Sphinx)
        // So with start-line: 2 on "foo\nbar\nbaz", we get only "baz"
        let temp_dir = TempDir::new().unwrap();
        let include_file = temp_dir.path().join("included.rst");
        std::fs::write(&include_file, "foo\nbar\nbaz\n").unwrap();

        // Create an RST file that includes with start-line: 2
        let rst_content = r#"Title
=====

.. include:: included.rst
   :start-line: 2
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let mut parser = Parser::new(&config).unwrap();
        parser.set_source_dir(temp_dir.path().to_path_buf());
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // With start-line: 2, we skip the first 2 lines (foo, bar) and get only baz
        assert!(
            html.contains("baz"),
            "should contain 'baz', got: {}",
            html
        );
        assert!(
            !html.contains("foo"),
            "should NOT contain 'foo', got: {}",
            html
        );
        assert!(
            !html.contains("bar"),
            "should NOT contain 'bar', got: {}",
            html
        );
    }

    #[test]
    fn test_include_file_not_found() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();

        // Create an RST file that tries to include a non-existent file
        let rst_content = r#"Title
=====

.. include:: nonexistent.rst
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let mut parser = Parser::new(&config).unwrap();
        parser.set_source_dir(temp_dir.path().to_path_buf());
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // When include file is not found during parsing, it's silently ignored
        // The document should still render, just without the included content
        assert!(
            html.contains("Title"),
            "should still contain the title, got: {}",
            html
        );
    }

    #[test]
    fn test_include_header_levels_shared() {
        use crate::config::BuildConfig;
        use crate::parser::Parser;
        use tempfile::TempDir;

        // Test that header levels are correctly shared between main doc and included content
        let temp_dir = TempDir::new().unwrap();

        // The included file has a header with = underline
        let include_file = temp_dir.path().join("included.rst");
        std::fs::write(&include_file, "Included Section\n================\n\nIncluded content.\n").unwrap();

        // Main doc has = for level 1, - for level 2
        // The included file's = header should become level 1 (same as main doc's =)
        let rst_content = r#"Main Title
==========

Some content.

Sub Section
-----------

More content.

.. include:: included.rst
"#;

        let rst_file = temp_dir.path().join("doc.rst");
        std::fs::write(&rst_file, rst_content).unwrap();

        let config = BuildConfig::default();
        let mut parser = Parser::new(&config).unwrap();
        parser.set_source_dir(temp_dir.path().to_path_buf());
        let doc = parser.parse(&rst_file, rst_content).unwrap();

        let renderer = HtmlRenderer::new();
        let html = renderer.render_document_content(&doc.content);

        // The included section should be h1 (level 1) since it uses = which is already level 1
        assert!(
            html.contains("<h1>Included Section"),
            "included section should be h1, got: {}",
            html
        );
        // Sub Section should be h2 (level 2)
        assert!(
            html.contains("<h2>Sub Section"),
            "sub section should be h2, got: {}",
            html
        );
    }
}
