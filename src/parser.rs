use anyhow::Result;
use log::debug;
use pulldown_cmark::{Event, Parser as MarkdownParser, Tag};
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;

use crate::config::BuildConfig;
use crate::directives::DirectiveRegistry;
use crate::document::{
    CrossReference, Document, DocumentContent, MarkdownContent, MarkdownNode, RstContent,
    RstDirective, RstNode, TocEntry,
};
// use crate::roles::RoleRegistry; // TODO: Implement roles module
use crate::utils;

pub struct Parser {
    rst_directive_regex: Regex,
    cross_ref_regex: Regex,
    #[allow(dead_code)]
    directive_registry: DirectiveRegistry,
    // #[allow(dead_code)]
    // role_registry: RoleRegistry, // TODO: Implement roles module
}

impl Parser {
    pub fn new(_config: &BuildConfig) -> Result<Self> {
        // Match directive names with hyphens (e.g., code-block, csv-table)
        let rst_directive_regex = Regex::new(r"^\s*\.\.\s+([\w-]+)::\s*(.*?)$")?;
        let cross_ref_regex = Regex::new(r":(\w+):`([^`]+)`")?;
        let directive_registry = DirectiveRegistry::new();
        // let role_registry = RoleRegistry::new(); // TODO: Implement roles module

        Ok(Self {
            rst_directive_regex,
            cross_ref_regex,
            directive_registry,
            // role_registry, // TODO: Implement roles module
        })
    }

    pub fn parse(&self, file_path: &Path, content: &str) -> Result<Document> {
        let output_path = self.get_output_path(file_path)?;
        let mut document = Document::new(file_path.to_path_buf(), output_path);

        // Set source modification time
        document.source_mtime = utils::get_file_mtime(file_path)?;

        // Determine file type and parse accordingly
        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");

        match extension {
            "rst" => {
                document.content = self.parse_rst(content)?;
            }
            "md" => {
                document.content = self.parse_markdown(content)?;
            }
            _ => {
                document.content = DocumentContent::PlainText(content.to_string());
            }
        }

        // Extract title from content
        document.title = self.extract_title(&document.content);

        // Extract table of contents
        document.toc = self.extract_toc(&document.content);

        // Extract cross-references
        document.cross_refs = self.extract_cross_refs(content);

        debug!(
            "Parsed document: {} ({} chars)",
            file_path.display(),
            content.len()
        );

        Ok(document)
    }

    fn parse_rst(&self, content: &str) -> Result<DocumentContent> {
        let mut nodes = Vec::new();
        let mut directives = Vec::new();
        let lines: Vec<&str> = content.lines().collect();

        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();

            if trimmed.is_empty() {
                i += 1;
                continue;
            }

            // Check for RST directive
            if let Some(captures) = self.rst_directive_regex.captures(line) {
                let directive_name = captures.get(1).unwrap().as_str();
                let directive_args = captures.get(2).unwrap().as_str();

                let (directive, consumed_lines) =
                    self.parse_rst_directive(&lines[i..], directive_name, directive_args, i + 1)?;

                directives.push(directive.clone());
                nodes.push(RstNode::Directive {
                    name: directive.name,
                    args: directive.args,
                    options: directive.options,
                    content: directive.content,
                    line: i + 1,
                });

                i += consumed_lines;
                continue;
            }

            // Check for title (underlined with =, -, ~, etc.)
            if i + 1 < lines.len() {
                let next_line = lines[i + 1];
                // Use chars().count() for proper Unicode character counting
                // (handles non-breaking spaces and other multi-byte characters)
                let title_char_count = trimmed.chars().count();
                let underline_char_count = next_line.trim().chars().count();

                if !next_line.trim().is_empty()
                    && next_line.trim().chars().all(|c| "=-~^\"'*+#<>".contains(c))
                    && underline_char_count >= title_char_count
                {
                    let level = self.get_rst_title_level(next_line.trim().chars().next().unwrap());
                    nodes.push(RstNode::Title {
                        text: trimmed.to_string(),
                        level,
                        line: i + 1,
                    });

                    i += 2;
                    continue;
                }
            }

            // Check for code block (indented text after ::)
            if line.ends_with("::") {
                let (code_content, consumed_lines) = self.parse_code_block(&lines[i + 1..]);
                nodes.push(RstNode::CodeBlock {
                    language: None,
                    content: code_content,
                    line: i + 1,
                });
                i += consumed_lines + 1;
                continue;
            }

            // Check for internal hyperlink target (.. _link-name:)
            if let Some(target_name) = self.parse_link_target(trimmed) {
                nodes.push(RstNode::LinkTarget {
                    name: target_name,
                    line: i + 1,
                });
                i += 1;
                continue;
            }

            // Check for RST comment (lines starting with ".. " that aren't directives or link targets)
            // Comments can span multiple lines if subsequent lines are indented
            if trimmed.starts_with(".. ") {
                i += 1;
                // Skip any following indented lines that are part of the comment
                while i < lines.len() {
                    let next_line = lines[i];
                    if next_line.trim().is_empty()
                        || next_line.starts_with("   ")
                        || next_line.starts_with("\t")
                    {
                        i += 1;
                    } else {
                        break;
                    }
                }
                continue;
            }

            // Check for block quote (indented text that isn't part of a directive)
            // Block quotes start with indentation (at least 3 spaces or a tab)
            if line.starts_with("   ") || line.starts_with("\t") {
                let (blockquote_content, consumed_lines) = self.parse_blockquote(&lines[i..]);
                if !blockquote_content.trim().is_empty() {
                    nodes.push(RstNode::BlockQuote {
                        content: blockquote_content,
                        line: i + 1,
                    });
                }
                i += consumed_lines;
                continue;
            }

            // Default to paragraph
            let (paragraph_content, consumed_lines) = self.parse_paragraph(&lines[i..]);
            nodes.push(RstNode::Paragraph {
                content: paragraph_content,
                line: i + 1,
            });
            i += consumed_lines;
        }

        Ok(DocumentContent::RestructuredText(RstContent {
            raw: content.to_string(),
            ast: nodes,
            directives,
        }))
    }

    fn parse_markdown(&self, content: &str) -> Result<DocumentContent> {
        let mut nodes = Vec::new();
        let parser = MarkdownParser::new(content);
        let current_line = 1;

        for event in parser {
            match event {
                Event::Start(Tag::Heading { .. }) => {
                    // We'll handle this in the text event
                }
                Event::End(_) => {
                    // Handle end tags generically
                }
                Event::Start(Tag::Paragraph) => {
                    // Start of paragraph
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    // Start of code block
                }
                Event::Text(text) => {
                    // Handle text content based on context
                    nodes.push(MarkdownNode::Paragraph {
                        content: text.to_string(),
                        line: current_line,
                    });
                }
                Event::Code(_code) => {
                    // Inline code
                }
                _ => {
                    // Handle other events as needed
                }
            }
        }

        Ok(DocumentContent::Markdown(MarkdownContent {
            raw: content.to_string(),
            ast: nodes,
            front_matter: None, // TODO: Parse YAML front matter
        }))
    }

    fn parse_rst_directive(
        &self,
        lines: &[&str],
        name: &str,
        args: &str,
        start_line: usize,
    ) -> Result<(RstDirective, usize)> {
        let mut options = HashMap::new();
        let mut content = String::new();
        let mut consumed_lines = 1;
        let mut i = 1;

        // Parse options (lines starting with :option:)
        while i < lines.len() {
            let line = lines[i];
            if line.trim().is_empty() {
                i += 1;
                consumed_lines += 1;
                continue;
            }

            if let Some(stripped) = line.strip_prefix("   :") {
                // This is an option
                if let Some(colon_pos) = stripped.find(':') {
                    let option_name = &stripped[..colon_pos];
                    let option_value = stripped[colon_pos + 1..].trim();
                    options.insert(option_name.to_string(), option_value.to_string());
                }
                i += 1;
                consumed_lines += 1;
            } else if line.starts_with("   ") || line.starts_with("\t") {
                // This is content
                break;
            } else {
                // End of directive
                break;
            }
        }

        // Parse content (indented lines)
        while i < lines.len() {
            let line = lines[i];
            if line.starts_with("   ") || line.starts_with("\t") {
                content.push_str(&line[3..]); // Remove 3 spaces of indentation
                content.push('\n');
                i += 1;
                consumed_lines += 1;
            } else if line.trim().is_empty() {
                content.push('\n');
                i += 1;
                consumed_lines += 1;
            } else {
                break;
            }
        }

        let directive = RstDirective {
            name: name.to_string(),
            args: if args.is_empty() {
                Vec::new()
            } else {
                vec![args.to_string()]
            },
            options,
            content: content.trim_end().to_string(),
            line: start_line,
        };

        Ok((directive, consumed_lines))
    }

    fn get_rst_title_level(&self, char: char) -> usize {
        match char {
            '#' => 1,
            '*' => 2,
            '=' => 3,
            '-' => 4,
            '^' => 5,
            '"' => 6,
            _ => 7,
        }
    }

    fn parse_code_block(&self, lines: &[&str]) -> (String, usize) {
        let mut content = String::new();
        let mut consumed_lines = 0;

        for line in lines {
            if line.starts_with("   ") || line.starts_with("\t") || line.trim().is_empty() {
                content.push_str(line);
                content.push('\n');
                consumed_lines += 1;
            } else {
                break;
            }
        }

        (content.trim().to_string(), consumed_lines)
    }

    fn parse_paragraph(&self, lines: &[&str]) -> (String, usize) {
        let mut content = String::new();
        let mut consumed_lines = 0;

        for line in lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }

            content.push_str(trimmed);
            content.push(' ');
            consumed_lines += 1;
        }

        (content.trim().to_string(), consumed_lines)
    }

    fn parse_blockquote(&self, lines: &[&str]) -> (String, usize) {
        let mut content = String::new();
        let mut consumed_lines = 0;

        for line in lines {
            // Block quote continues while lines are indented or empty
            if line.starts_with("   ") || line.starts_with("\t") {
                // Remove the leading indentation (3 spaces or 1 tab)
                let dedented = if line.starts_with("   ") {
                    &line[3..]
                } else {
                    &line[1..]
                };
                content.push_str(dedented);
                content.push('\n');
                consumed_lines += 1;
            } else if line.trim().is_empty() {
                // Empty lines can be part of the block quote if more indented content follows
                // But we'll stop at empty lines for simplicity (can be enhanced later)
                consumed_lines += 1;
                break;
            } else {
                // Non-indented non-empty line ends the block quote
                break;
            }
        }

        (content.trim().to_string(), consumed_lines)
    }

    /// Parse an internal hyperlink target like `.. _link-name:`
    /// Returns the target name if this is a valid link target, None otherwise.
    fn parse_link_target(&self, line: &str) -> Option<String> {
        // Pattern: .. _name: (where name can contain letters, numbers, hyphens, underscores)
        let trimmed = line.trim();
        if trimmed.starts_with(".. _") && trimmed.ends_with(':') {
            let name = &trimmed[4..trimmed.len() - 1]; // Remove ".. _" prefix and ":" suffix
            if !name.is_empty() && !name.contains(' ') {
                return Some(name.to_string());
            }
        }
        None
    }

    fn extract_title(&self, content: &DocumentContent) -> String {
        match content {
            DocumentContent::RestructuredText(rst) => {
                // In RST, the first title in the document is the document title,
                // regardless of which underline character is used
                for node in &rst.ast {
                    if let RstNode::Title { text, .. } = node {
                        return text.clone();
                    }
                }
            }
            DocumentContent::Markdown(md) => {
                for node in &md.ast {
                    if let MarkdownNode::Heading { text, level: 1, .. } = node {
                        return text.clone();
                    }
                }
            }
            DocumentContent::PlainText(_) => {}
        }

        "Untitled".to_string()
    }

    fn extract_toc(&self, content: &DocumentContent) -> Vec<TocEntry> {
        let mut toc = Vec::new();

        match content {
            DocumentContent::RestructuredText(rst) => {
                for node in &rst.ast {
                    if let RstNode::Title { text, level, line } = node {
                        let anchor = text.to_lowercase().replace(' ', "-");
                        toc.push(TocEntry::new(text.clone(), *level, anchor, *line));
                    }
                }
            }
            DocumentContent::Markdown(md) => {
                for node in &md.ast {
                    if let MarkdownNode::Heading { text, level, line } = node {
                        let anchor = text.to_lowercase().replace(' ', "-");
                        toc.push(TocEntry::new(text.clone(), *level, anchor, *line));
                    }
                }
            }
            DocumentContent::PlainText(_) => {}
        }

        toc
    }

    fn extract_cross_refs(&self, content: &str) -> Vec<CrossReference> {
        let mut cross_refs = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            for captures in self.cross_ref_regex.captures_iter(line) {
                let ref_type = captures.get(1).unwrap().as_str();
                let target = captures.get(2).unwrap().as_str();

                cross_refs.push(CrossReference {
                    ref_type: ref_type.to_string(),
                    target: target.to_string(),
                    text: None,
                    line_number: line_num + 1,
                });
            }
        }

        cross_refs
    }

    fn get_output_path(&self, source_path: &Path) -> Result<std::path::PathBuf> {
        let mut output_path = source_path.to_path_buf();
        output_path.set_extension("html");
        Ok(output_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_parser() -> Parser {
        let config = crate::config::BuildConfig::default();
        Parser::new(&config).unwrap()
    }

    fn parse_rst_content(parser: &Parser, content: &str) -> Document {
        let mut temp_file = NamedTempFile::with_suffix(".rst").unwrap();
        temp_file.write_all(content.as_bytes()).unwrap();
        temp_file.flush().unwrap();
        parser.parse(temp_file.path(), content).unwrap()
    }

    #[test]
    fn test_title_with_equals_underline() {
        let parser = create_parser();
        let content = "My Title\n========\n\nSome text.";
        let doc = parse_rst_content(&parser, content);

        assert_eq!(doc.title, "My Title");
    }

    #[test]
    fn test_title_with_dash_underline() {
        let parser = create_parser();
        let content = "My Title\n--------\n\nSome text.";
        let doc = parse_rst_content(&parser, content);

        assert_eq!(doc.title, "My Title");
    }

    #[test]
    fn test_title_with_tilde_underline() {
        let parser = create_parser();
        let content = "My Title\n~~~~~~~~\n\nSome text.";
        let doc = parse_rst_content(&parser, content);

        assert_eq!(doc.title, "My Title");
    }

    #[test]
    fn test_title_with_caret_underline() {
        let parser = create_parser();
        let content = "My Title\n^^^^^^^^\n\nSome text.";
        let doc = parse_rst_content(&parser, content);

        assert_eq!(doc.title, "My Title");
    }

    #[test]
    fn test_title_with_hash_underline() {
        let parser = create_parser();
        let content = "My Title\n########\n\nSome text.";
        let doc = parse_rst_content(&parser, content);

        assert_eq!(doc.title, "My Title");
    }

    #[test]
    fn test_title_with_asterisk_underline() {
        let parser = create_parser();
        let content = "My Title\n********\n\nSome text.";
        let doc = parse_rst_content(&parser, content);

        assert_eq!(doc.title, "My Title");
    }

    #[test]
    fn test_title_levels() {
        let parser = create_parser();

        // # is level 1
        assert_eq!(parser.get_rst_title_level('#'), 1);
        // * is level 2
        assert_eq!(parser.get_rst_title_level('*'), 2);
        // = is level 3
        assert_eq!(parser.get_rst_title_level('='), 3);
        // - is level 4
        assert_eq!(parser.get_rst_title_level('-'), 4);
        // ^ is level 5
        assert_eq!(parser.get_rst_title_level('^'), 5);
        // " is level 6
        assert_eq!(parser.get_rst_title_level('"'), 6);
    }

    #[test]
    fn test_multiple_titles_with_different_underlines() {
        let parser = create_parser();
        let content = r#"Main Title
==========

Some intro text.

Subsection
----------

More text.

Sub-subsection
^^^^^^^^^^^^^^

Even more text.
"#;
        let doc = parse_rst_content(&parser, content);

        // First title becomes the document title
        assert_eq!(doc.title, "Main Title");
    }

    #[test]
    fn test_title_with_inline_markup_and_caret_underline() {
        let parser = create_parser();
        let content = r#"`attrs`       (:ref:`evaluated <evaluate>`)
^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

Type: :doc:`Attrs`

    See :ref:`attributes <attributes>`
"#;
        let doc = parse_rst_content(&parser, content);

        // Should recognize the title with inline markup
        assert_eq!(doc.title, "`attrs`       (:ref:`evaluated <evaluate>`)");

        // Count the titles in the AST
        if let crate::document::DocumentContent::RestructuredText(rst) = &doc.content {
            let title_count = rst.ast.iter().filter(|node| {
                matches!(node, RstNode::Title { .. })
            }).count();
            assert_eq!(title_count, 1, "Should have exactly one title");
        } else {
            panic!("Expected RST content");
        }
    }

    #[test]
    fn test_title_with_non_breaking_spaces() {
        let parser = create_parser();
        // Use actual non-breaking spaces (U+00A0) between `attrs` and (:ref:
        let content = "`attrs`\u{00A0}\u{00A0}\u{00A0}\u{00A0}\u{00A0}\u{00A0}\u{00A0}(:ref:`evaluated <evaluate>`)\n^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n\nType: :doc:`Attrs`\n";
        let doc = parse_rst_content(&parser, content);

        // Should still recognize the title
        assert!(!doc.title.is_empty() && doc.title != "Untitled",
            "Title should be recognized, got: {}", doc.title);

        // Count the titles in the AST
        if let crate::document::DocumentContent::RestructuredText(rst) = &doc.content {
            let title_count = rst.ast.iter().filter(|node| {
                matches!(node, RstNode::Title { .. })
            }).count();
            assert_eq!(title_count, 1, "Should have exactly one title, got {}", title_count);
        } else {
            panic!("Expected RST content");
        }
    }
}
