//! Navigation and document hierarchy management.
//!
//! This module provides structures for tracking document relationships
//! (parent, children, prev, next) and building the navigation tree for sidebars.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Process inline markup in navigation titles (backticks -> code tags)
fn render_nav_title(title: &str) -> String {
    // First HTML escape the content
    let escaped = html_escape::encode_text(title).to_string();

    // Process single backticks: `code` -> <code class="code docutils literal notranslate"><span class="pre">code</span></code>
    let code_re = Regex::new(r"`([^`]+)`").unwrap();
    code_re
        .replace_all(&escaped, r#"<code class="code docutils literal notranslate"><span class="pre">$1</span></code>"#)
        .to_string()
}

/// Represents a navigation link
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavLink {
    pub title: String,
    pub link: String,
}

impl NavLink {
    pub fn new(title: impl Into<String>, link: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            link: link.into(),
        }
    }
}

/// Navigation context for a single page
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageNavigation {
    /// Parent documents (breadcrumb trail)
    pub parents: Vec<NavLink>,
    /// Previous document in reading order
    pub prev: Option<NavLink>,
    /// Next document in reading order
    pub next: Option<NavLink>,
    /// Children documents (for toctree)
    pub children: Vec<NavLink>,
}

/// A node in the document tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TocTreeNode {
    pub doc_path: String,
    pub title: String,
    pub children: Vec<TocTreeNode>,
}

impl TocTreeNode {
    pub fn new(doc_path: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            doc_path: doc_path.into(),
            title: title.into(),
            children: Vec::new(),
        }
    }

    /// Get all documents in reading order (depth-first)
    pub fn flatten(&self) -> Vec<(&str, &str)> {
        let mut result = vec![(self.doc_path.as_str(), self.title.as_str())];
        for child in &self.children {
            result.extend(child.flatten());
        }
        result
    }
}

/// Manages the document hierarchy and navigation
#[derive(Debug, Default)]
pub struct NavigationBuilder {
    /// Map from document path to its toctree children
    toctree_entries: HashMap<String, Vec<String>>,
    /// Map from document path to its title
    titles: HashMap<String, String>,
    /// The root document (usually "index")
    master_doc: String,
}

impl NavigationBuilder {
    pub fn new(master_doc: impl Into<String>) -> Self {
        Self {
            toctree_entries: HashMap::new(),
            titles: HashMap::new(),
            master_doc: master_doc.into(),
        }
    }

    /// Register a document with its title
    pub fn register_document(&mut self, doc_path: &str, title: &str) {
        self.titles.insert(doc_path.to_string(), title.to_string());
    }

    /// Register toctree entries for a document
    pub fn register_toctree(&mut self, doc_path: &str, entries: Vec<String>) {
        self.toctree_entries.insert(doc_path.to_string(), entries);
    }

    /// Build the document tree starting from the master document
    pub fn build_tree(&self) -> TocTreeNode {
        self.build_tree_for(&self.master_doc)
    }

    fn build_tree_for(&self, doc_path: &str) -> TocTreeNode {
        let title = self.titles.get(doc_path).cloned().unwrap_or_else(|| doc_path.to_string());
        let mut node = TocTreeNode::new(doc_path, title);

        if let Some(entries) = self.toctree_entries.get(doc_path) {
            for entry in entries {
                // Handle explicit title syntax: "Title <path>"
                let (child_title, child_path) = if let Some(angle_pos) = entry.find('<') {
                    if entry.ends_with('>') {
                        let title = entry[..angle_pos].trim();
                        let path = &entry[angle_pos + 1..entry.len() - 1];
                        (Some(title.to_string()), path.to_string())
                    } else {
                        (None, entry.clone())
                    }
                } else {
                    (None, entry.clone())
                };

                // For external URLs, create a leaf node (no recursive building)
                if child_path.starts_with("http://") || child_path.starts_with("https://") {
                    let ext_title = child_title.unwrap_or_else(|| child_path.clone());
                    let ext_node = TocTreeNode::new(&child_path, ext_title);
                    node.children.push(ext_node);
                    continue;
                }

                let mut child_node = self.build_tree_for(&child_path);
                // Use explicit title if provided
                if let Some(t) = child_title {
                    child_node.title = t;
                }
                node.children.push(child_node);
            }
        }

        node
    }

    /// Get navigation context for a specific document
    pub fn get_page_navigation(&self, doc_path: &str) -> PageNavigation {
        let tree = self.build_tree();
        let flat_docs = tree.flatten();

        let mut nav = PageNavigation::default();

        // Find position in flattened list for prev/next
        let position = flat_docs.iter().position(|(path, _)| *path == doc_path);

        if let Some(pos) = position {
            // Previous
            if pos > 0 {
                let (prev_path, prev_title) = flat_docs[pos - 1];
                nav.prev = Some(NavLink::new(
                    prev_title,
                    format!("{}.html", prev_path),
                ));
            }

            // Next
            if pos + 1 < flat_docs.len() {
                let (next_path, next_title) = flat_docs[pos + 1];
                nav.next = Some(NavLink::new(
                    next_title,
                    format!("{}.html", next_path),
                ));
            }
        }

        // Build parent chain
        nav.parents = self.find_parents(doc_path, &tree);

        // Get direct children
        if let Some(entries) = self.toctree_entries.get(doc_path) {
            for entry in entries {
                let (child_title, child_path) = if let Some(angle_pos) = entry.find('<') {
                    if entry.ends_with('>') {
                        let title = entry[..angle_pos].trim().to_string();
                        let path = entry[angle_pos + 1..entry.len() - 1].to_string();
                        (title, path)
                    } else {
                        let title = self.titles.get(entry).cloned().unwrap_or_else(|| entry.clone());
                        (title, entry.clone())
                    }
                } else {
                    let title = self.titles.get(entry).cloned().unwrap_or_else(|| entry.clone());
                    (title, entry.clone())
                };

                // Skip external URLs
                if !child_path.starts_with("http://") && !child_path.starts_with("https://") {
                    nav.children.push(NavLink::new(child_title, format!("{}.html", child_path)));
                }
            }
        }

        nav
    }

    fn find_parents(&self, doc_path: &str, tree: &TocTreeNode) -> Vec<NavLink> {
        let mut path = Vec::new();
        self.find_path_to(doc_path, tree, &mut path);
        // Remove the document itself from the path
        if !path.is_empty() {
            path.pop();
        }
        path
    }

    fn find_path_to(&self, target: &str, node: &TocTreeNode, path: &mut Vec<NavLink>) -> bool {
        path.push(NavLink::new(&node.title, format!("{}.html", &node.doc_path)));

        if node.doc_path == target {
            return true;
        }

        for child in &node.children {
            if self.find_path_to(target, child, path) {
                return true;
            }
        }

        path.pop();
        false
    }

    /// Render the toctree as HTML for templates (starts from root's children)
    pub fn render_toctree(&self, options: &ToctreeOptions) -> String {
        let tree = self.build_tree();

        // Build the path to current doc for "current" class markers
        let current_path = if let Some(ref current_doc) = options.current_doc {
            self.get_path_to_doc(current_doc, &tree)
        } else {
            Vec::new()
        };

        // Start from root's children, not root itself
        if tree.children.is_empty() {
            return String::new();
        }

        let mut checkbox_id = 1;
        let mut html = String::new();
        for child in &tree.children {
            html.push_str(&self.render_toctree_node(child, 1, options, &current_path, &mut checkbox_id));
        }
        html
    }

    /// Render toctree for a specific document (its children)
    pub fn render_toctree_for(&self, doc_path: &str, options: &ToctreeOptions) -> String {
        let tree = self.build_tree();

        let current_path = if let Some(ref current_doc) = options.current_doc {
            self.get_path_to_doc(current_doc, &tree)
        } else {
            Vec::new()
        };

        // Find the node for this document
        if let Some(node) = self.find_node(&tree, doc_path) {
            if node.children.is_empty() {
                return String::new();
            }

            let mut checkbox_id = 1;
            let mut html = String::from("<ul>\n");
            for child in &node.children {
                html.push_str(&self.render_toctree_node(child, 1, options, &current_path, &mut checkbox_id));
            }
            html.push_str("</ul>\n");
            return html;
        }

        String::new()
    }

    /// Get the path from root to a specific document (for current markers)
    fn get_path_to_doc(&self, doc_path: &str, tree: &TocTreeNode) -> Vec<String> {
        let mut path = Vec::new();
        self.find_doc_path(doc_path, tree, &mut path);
        path
    }

    fn find_doc_path(&self, target: &str, node: &TocTreeNode, path: &mut Vec<String>) -> bool {
        path.push(node.doc_path.clone());

        if node.doc_path == target {
            return true;
        }

        for child in &node.children {
            if self.find_doc_path(target, child, path) {
                return true;
            }
        }

        path.pop();
        false
    }

    fn find_node<'a>(&self, tree: &'a TocTreeNode, doc_path: &str) -> Option<&'a TocTreeNode> {
        if tree.doc_path == doc_path {
            return Some(tree);
        }
        for child in &tree.children {
            if let Some(found) = self.find_node(child, doc_path) {
                return Some(found);
            }
        }
        None
    }

    fn render_toctree_node(
        &self,
        node: &TocTreeNode,
        depth: usize,
        options: &ToctreeOptions,
        current_path: &[String],
        checkbox_id: &mut usize,
    ) -> String {
        if depth > options.maxdepth && options.maxdepth > 0 {
            return String::new();
        }

        let is_external = node.doc_path.starts_with("http://") || node.doc_path.starts_with("https://");
        let has_children = !node.children.is_empty() && (options.maxdepth == 0 || depth < options.maxdepth);
        let is_current = !is_external && current_path.contains(&node.doc_path);
        let is_current_page = !is_external && options.current_doc.as_ref().map(|d| d == &node.doc_path).unwrap_or(false);

        // Build class list
        let mut classes = vec![format!("toctree-l{}", depth)];
        if is_current {
            classes.push("current".to_string());
        }
        if is_current_page {
            classes.push("current-page".to_string());
        }
        if has_children {
            classes.push("has-children".to_string());
        }

        // Build link class and href
        let (link_class, href) = if is_external {
            ("reference external", node.doc_path.clone())
        } else if is_current_page {
            ("current reference internal", format!("{}.html", node.doc_path))
        } else {
            ("reference internal", format!("{}.html", node.doc_path))
        };

        let mut html = format!(
            "<li class=\"{}\"><a class=\"{}\" href=\"{}\">{}</a>",
            classes.join(" "),
            link_class,
            html_escape::encode_text(&href),
            render_nav_title(&node.title)
        );

        if has_children {
            // Add checkbox toggle for collapsible navigation
            let current_checkbox_id = *checkbox_id;
            *checkbox_id += 1;

            html.push_str(&format!(
                "<input aria-label=\"Toggle navigation of {}\" class=\"toctree-checkbox\" id=\"toctree-checkbox-{}\" name=\"toctree-checkbox-{}\" role=\"switch\" type=\"checkbox\"{}>",
                html_escape::encode_text(&node.title),
                current_checkbox_id,
                current_checkbox_id,
                if is_current { " checked" } else { "" }
            ));
            html.push_str(&format!(
                "<label for=\"toctree-checkbox-{}\"><span class=\"icon\"><svg><use href=\"#svg-arrow-right\"></use></svg></span></label>",
                current_checkbox_id
            ));

            html.push_str("<ul>\n");
            for child in &node.children {
                html.push_str(&self.render_toctree_node(child, depth + 1, options, current_path, checkbox_id));
            }
            html.push_str("</ul>\n");
        }

        html.push_str("</li>\n");
        html
    }

    /// Get the master document path
    pub fn master_doc(&self) -> &str {
        &self.master_doc
    }

    /// Get all registered titles
    pub fn titles(&self) -> &HashMap<String, String> {
        &self.titles
    }
}

/// Options for rendering toctree
#[derive(Debug, Clone)]
pub struct ToctreeOptions {
    pub maxdepth: usize,
    pub collapse: bool,
    pub includehidden: bool,
    pub titles_only: bool,
    /// The current document being rendered (for highlighting)
    pub current_doc: Option<String>,
}

impl Default for ToctreeOptions {
    fn default() -> Self {
        Self {
            maxdepth: 4,
            collapse: true,
            includehidden: true,
            titles_only: false,
            current_doc: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_navigation_builder() {
        let mut builder = NavigationBuilder::new("index");

        builder.register_document("index", "Welcome");
        builder.register_document("intro", "Introduction");
        builder.register_document("guide", "User Guide");
        builder.register_document("api", "API Reference");

        builder.register_toctree("index", vec!["intro".to_string(), "guide".to_string(), "api".to_string()]);

        let tree = builder.build_tree();
        assert_eq!(tree.title, "Welcome");
        assert_eq!(tree.children.len(), 3);
    }

    #[test]
    fn test_page_navigation() {
        let mut builder = NavigationBuilder::new("index");

        builder.register_document("index", "Welcome");
        builder.register_document("intro", "Introduction");
        builder.register_document("guide", "User Guide");

        builder.register_toctree("index", vec!["intro".to_string(), "guide".to_string()]);

        let nav = builder.get_page_navigation("intro");

        // intro should have prev (index) and next (guide)
        assert!(nav.prev.is_some());
        assert_eq!(nav.prev.as_ref().unwrap().title, "Welcome");

        assert!(nav.next.is_some());
        assert_eq!(nav.next.as_ref().unwrap().title, "User Guide");

        // intro should have index as parent
        assert_eq!(nav.parents.len(), 1);
        assert_eq!(nav.parents[0].title, "Welcome");
    }

    #[test]
    fn test_explicit_title_syntax() {
        let mut builder = NavigationBuilder::new("index");

        builder.register_document("index", "Welcome");
        builder.register_document("intro", "Introduction");

        builder.register_toctree("index", vec!["Getting Started <intro>".to_string()]);

        let tree = builder.build_tree();
        assert_eq!(tree.children[0].title, "Getting Started");
        assert_eq!(tree.children[0].doc_path, "intro");
    }

    #[test]
    fn test_render_toctree() {
        let mut builder = NavigationBuilder::new("index");

        builder.register_document("index", "Welcome");
        builder.register_document("intro", "Introduction");
        builder.register_document("guide", "User Guide");

        builder.register_toctree("index", vec!["intro".to_string(), "guide".to_string()]);

        let options = ToctreeOptions::default();
        let html = builder.render_toctree(&options);

        // Should contain children of root (intro and guide), but NOT the root (Welcome)
        assert!(html.contains("Introduction"));
        assert!(html.contains("User Guide"));
        assert!(html.contains("intro.html"));
        assert!(html.contains("guide.html"));
        assert!(!html.contains("Welcome")); // Root should not be in toctree
    }

    #[test]
    fn test_render_toctree_with_current_doc() {
        let mut builder = NavigationBuilder::new("index");

        builder.register_document("index", "Welcome");
        builder.register_document("components", "Components");
        builder.register_document("action", "Action");

        builder.register_toctree("index", vec!["components".to_string()]);
        builder.register_toctree("components", vec!["action".to_string()]);

        let mut options = ToctreeOptions::default();
        options.current_doc = Some("action".to_string());
        let html = builder.render_toctree(&options);

        // Should have current classes in the path to action
        assert!(html.contains("class=\"toctree-l1 current has-children\""));
        assert!(html.contains("class=\"toctree-l2 current current-page\""));
        assert!(html.contains("class=\"current reference internal\" href=\"action.html\""));
        // Should have checkbox toggle
        assert!(html.contains("toctree-checkbox"));
    }

    #[test]
    fn test_render_toctree_has_children() {
        let mut builder = NavigationBuilder::new("index");

        builder.register_document("index", "Welcome");
        builder.register_document("parent", "Parent");
        builder.register_document("child", "Child");
        builder.register_document("leaf", "Leaf");

        builder.register_toctree("index", vec!["parent".to_string(), "leaf".to_string()]);
        builder.register_toctree("parent", vec!["child".to_string()]);

        let options = ToctreeOptions::default();
        let html = builder.render_toctree(&options);

        // Parent has children, leaf does not
        assert!(html.contains("has-children"));
        assert!(html.contains("<li class=\"toctree-l1\"><a class=\"reference internal\" href=\"leaf.html\">Leaf</a></li>"));
    }
}
