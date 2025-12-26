use anyhow::{Context, Result};
use log::{debug, info, warn};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cache::BuildCache;
use crate::config::BuildConfig;
use crate::document::Document;
use crate::error::{BuildErrorReport, BuildWarning};
use crate::extensions::{ExtensionLoader, SphinxApp};
use crate::matching;
use crate::navigation::{NavigationBuilder, PageNavigation, ToctreeOptions};
use crate::parser::Parser;
use crate::renderer::HtmlRenderer;
use crate::template::{SafeHtml, TemplateContext, TemplateEngine};
use crate::theme::{Theme, ThemeRegistry};
use crate::utils;

#[derive(Debug, Clone)]
pub struct BuildStats {
    pub files_processed: usize,
    pub files_skipped: usize,
    pub build_time: Duration,
    pub output_size_mb: f64,
    pub cache_hits: usize,
    pub errors: usize,
    pub warnings: usize,
    pub warning_details: Vec<BuildWarning>,
    pub error_details: Vec<BuildErrorReport>,
}

/// NavLink with SafeHtml title for template rendering (no escaping needed)
#[derive(Debug, Clone, serde::Serialize)]
struct NavLinkSafe {
    title: SafeHtml,
    link: String,
}

impl NavLinkSafe {
    fn from_nav_link(link: &crate::navigation::NavLink) -> Self {
        Self {
            title: SafeHtml::new(&link.title),
            link: link.link.clone(),
        }
    }
}

pub struct SphinxBuilder {
    config: BuildConfig,
    source_dir: PathBuf,
    output_dir: PathBuf,
    cache: BuildCache,
    parser: Parser,
    parallel_jobs: usize,
    incremental: bool,
    warnings: Arc<Mutex<Vec<BuildWarning>>>,
    errors: Arc<Mutex<Vec<BuildErrorReport>>>,
    /// Map of document paths (without extension) to their titles
    document_titles: Arc<Mutex<HashMap<String, String>>>,
    /// Map of document paths to their sections (title, anchor) for nested toctree entries
    document_sections: Arc<Mutex<HashMap<String, Vec<(String, String)>>>>,
    #[allow(dead_code)]
    sphinx_app: Option<SphinxApp>,
    #[allow(dead_code)]
    extension_loader: ExtensionLoader,
    /// Theme registry for discovering themes
    #[allow(dead_code)]
    theme_registry: ThemeRegistry,
    /// The active theme
    active_theme: Option<Theme>,
    /// Navigation builder for document hierarchy
    navigation: Arc<Mutex<NavigationBuilder>>,
    /// Template engine for rendering HTML
    template_engine: TemplateEngine,
}

impl SphinxBuilder {
    pub fn new(config: BuildConfig, source_dir: PathBuf, output_dir: PathBuf) -> Result<Self> {
        let cache_dir = output_dir.join(".sphinx-ultra-cache");
        let cache = BuildCache::new(cache_dir)?;

        let mut parser = Parser::new(&config)?;
        parser.set_source_dir(source_dir.clone());

        let parallel_jobs = config.parallel_jobs.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });

        // Initialize Sphinx app with extensions
        let mut sphinx_app = SphinxApp::new(config.clone())?;
        let mut extension_loader = ExtensionLoader::new()?;

        // Load configured extensions
        for extension_name in &config.extensions {
            match extension_loader.load_extension(extension_name) {
                Ok(extension) => {
                    if let Err(e) = sphinx_app.add_extension(extension) {
                        log::warn!("Failed to add extension '{}': {}", extension_name, e);
                    }
                }
                Err(e) => {
                    log::warn!("Failed to load extension '{}': {}", extension_name, e);
                }
            }
        }

        // Initialize theme system
        let (theme_registry, active_theme) =
            Self::init_themes(&config, &source_dir)?;

        // Initialize navigation builder with root_doc (aka master_doc)
        let master_doc = config.root_doc.clone().unwrap_or_else(|| "index".to_string());
        let navigation = NavigationBuilder::new(master_doc);

        // Initialize template engine
        let template_engine = TemplateEngine::new(&config)?;

        Ok(Self {
            config,
            source_dir,
            output_dir,
            cache,
            parser,
            parallel_jobs,
            incremental: false,
            warnings: Arc::new(Mutex::new(Vec::new())),
            errors: Arc::new(Mutex::new(Vec::new())),
            document_titles: Arc::new(Mutex::new(HashMap::new())),
            document_sections: Arc::new(Mutex::new(HashMap::new())),
            sphinx_app: Some(sphinx_app),
            extension_loader,
            theme_registry,
            active_theme,
            navigation: Arc::new(Mutex::new(navigation)),
            template_engine,
        })
    }

    /// Initialize theme system - discover themes and find the configured theme
    fn init_themes(config: &BuildConfig, source_dir: &Path) -> Result<(ThemeRegistry, Option<Theme>)> {
        let mut registry = ThemeRegistry::new();

        // Add built-in themes directory relative to executable
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let themes_dir = exe_dir.join("themes");
                if themes_dir.exists() {
                    registry.add_search_path(themes_dir);
                }
            }
        }

        // Add themes directory relative to source directory
        let src_themes = source_dir.join("_themes");
        if src_themes.exists() {
            registry.add_search_path(src_themes);
        }

        // Add user-configured theme paths
        for theme_path in &config.theme.theme_paths {
            let abs_path = if theme_path.is_absolute() {
                theme_path.clone()
            } else {
                source_dir.join(theme_path)
            };
            if abs_path.exists() {
                registry.add_search_path(abs_path);
            }
        }

        // Discover themes in search paths
        registry.discover_themes()?;

        // Get the configured theme name
        let theme_name = &config.theme.name;

        // Try to find the theme: first in registry, then via Python
        let theme = if let Some(t) = registry.get_theme(theme_name) {
            Some(t.clone())
        } else {
            // Try to find via Python (pip-installed theme)
            if registry.discover_python_theme(theme_name)? {
                registry.get_theme(theme_name).cloned()
            } else {
                None
            }
        };

        match theme {
            Some(t) => {
                info!("Using theme '{}' from {}", t.name, t.path.display());
                Ok((registry, Some(t)))
            }
            None => Err(anyhow::anyhow!(
                "Theme '{}' not found. Searched in built-in themes, source directory, \
                 configured theme paths, and Python packages.",
                theme_name
            )),
        }
    }

    pub fn set_parallel_jobs(&mut self, jobs: usize) {
        self.parallel_jobs = jobs;
    }

    pub fn enable_incremental(&mut self) {
        self.incremental = true;
    }

    /// Add a warning to the collection
    #[allow(dead_code)]
    pub fn add_warning(&self, warning: BuildWarning) {
        self.warnings.lock().unwrap().push(warning);
    }

    /// Add an error to the collection
    #[allow(dead_code)]
    pub fn add_error(&self, error: BuildErrorReport) {
        self.errors.lock().unwrap().push(error);
    }

    /// Check if warnings should be treated as errors
    #[allow(dead_code)]
    pub fn should_fail_on_warning(&self) -> bool {
        self.config.fail_on_warning
    }

    pub async fn clean(&self) -> Result<()> {
        if self.output_dir.exists() {
            tokio::fs::remove_dir_all(&self.output_dir).await?;
        }
        Ok(())
    }

    /// Collect document titles and toctree entries from all source files (first pass).
    /// This is used to populate toctree entries with proper document titles and build navigation.
    fn collect_document_titles(&self, files: &[PathBuf]) -> Result<()> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.parallel_jobs)
            .build()?;

        // Pre-canonicalize output directory for comparison
        let canonical_output = self.output_dir.canonicalize().ok();

        // Collect titles and toctree entries
        let doc_info: Vec<_> = pool.install(|| {
            files
                .par_iter()
                .filter_map(|file_path| {
                    // Safety check: skip files that are inside the output directory
                    if let Some(ref output) = canonical_output {
                        if let Ok(canonical_file) = file_path.canonicalize() {
                            if canonical_file.starts_with(output) {
                                log::warn!(
                                    "Skipping file inside output directory: {}",
                                    file_path.display()
                                );
                                return None;
                            }
                        }
                    }

                    // Read and parse the file to extract its title
                    let content = std::fs::read_to_string(file_path).ok()?;
                    let doc = self.parser.parse(file_path, &content).ok()?;

                    // Get the document path relative to source dir, without extension
                    let relative_path = file_path.strip_prefix(&self.source_dir).ok()?;
                    let doc_path = relative_path
                        .with_extension("")
                        .to_string_lossy()
                        .replace('\\', "/"); // Normalize path separators

                    // Extract toctree entries
                    let toctree_entries = self.extract_toctree_references(&doc).unwrap_or_default();

                    // Extract sections (sub-titles) from the document for nested toctree entries
                    let sections = Self::extract_document_sections(&doc);

                    // Return doc info
                    let title = if !doc.title.is_empty() && doc.title != "Untitled" {
                        doc.title
                    } else {
                        doc_path.clone()
                    };

                    Some((doc_path, title, toctree_entries, sections))
                })
                .collect()
        });

        // Store collected titles, sections, and build navigation
        let mut doc_titles = self.document_titles.lock().unwrap();
        let mut doc_sections = self.document_sections.lock().unwrap();
        let mut nav = self.navigation.lock().unwrap();

        for (path, title, toctree_entries, sections) in doc_info {
            doc_titles.insert(path.clone(), title.clone());
            if !sections.is_empty() {
                doc_sections.insert(path.clone(), sections);
            }
            nav.register_document(&path, &title);
            if !toctree_entries.is_empty() {
                nav.register_toctree(&path, toctree_entries);
            }
        }

        Ok(())
    }

    /// Extract sections (sub-titles) from a document for nested toctree entries.
    /// Returns a vector of (title, anchor) tuples for level 2 headers.
    fn extract_document_sections(doc: &Document) -> Vec<(String, String)> {
        use crate::document::{DocumentContent, RstNode};
        use crate::renderer::slugify;

        let mut sections = Vec::new();

        if let DocumentContent::RestructuredText(rst) = &doc.content {
            for node in &rst.ast {
                if let RstNode::Title { text, level, .. } = node {
                    // Only include level 2 headers (immediate sub-sections)
                    if *level == 2 {
                        // Generate anchor/slug from title
                        let anchor = slugify(text);
                        sections.push((text.clone(), anchor));
                    }
                }
            }
        }

        sections
    }

    pub async fn build(&self) -> Result<BuildStats> {
        let start_time = Instant::now();
        info!("Starting build process...");

        // Ensure output directory exists
        tokio::fs::create_dir_all(&self.output_dir).await
            .with_context(|| format!("Failed to create output directory: {}", self.output_dir.display()))?;

        // Discover all source files
        let source_files = self.discover_source_files().await?;
        info!("Discovered {} source files", source_files.len());

        // Build dependency graph
        let dependency_graph = self.build_dependency_graph(&source_files).await?;
        debug!(
            "Built dependency graph with {} nodes",
            dependency_graph.len()
        );

        // First pass: Collect document titles for toctree rendering
        self.collect_document_titles(&source_files)?;
        debug!(
            "Collected {} document titles",
            self.document_titles.lock().unwrap().len()
        );

        // Process files in dependency order
        let processed_docs = self
            .process_files_parallel(&source_files, &dependency_graph)
            .await?;

        // Validate documents and collect warnings/errors
        self.validate_documents(&processed_docs, &source_files)
            .await?;

        // Generate cross-references and indices
        self.generate_indices(&processed_docs).await?;

        // Copy static assets
        self.copy_static_assets().await?;

        // Copy html_extra_path directories to output root
        self.copy_extra_paths().await?;

        // Generate sitemap and search index
        self.generate_search_index(&processed_docs).await?;

        let build_time = start_time.elapsed();
        let output_size = utils::calculate_directory_size(&self.output_dir).await?;

        let warnings = self.warnings.lock().unwrap();
        let errors = self.errors.lock().unwrap();

        let stats = BuildStats {
            files_processed: processed_docs.len(),
            files_skipped: 0, // TODO: Track skipped files
            build_time,
            output_size_mb: output_size as f64 / 1024.0 / 1024.0,
            cache_hits: self.cache.hit_count(),
            errors: errors.len(),
            warnings: warnings.len(),
            warning_details: warnings.clone(),
            error_details: errors.clone(),
        };

        info!("Build completed in {:?}", build_time);
        Ok(stats)
    }

    async fn discover_source_files(&self) -> Result<Vec<PathBuf>> {
        // Use pattern-based file discovery like Sphinx
        let mut include_patterns = self.config.include_patterns.clone();
        let exclude_patterns = &self.config.exclude_patterns;

        // Add default source file patterns if no specific patterns are configured
        if include_patterns == vec!["**"] {
            include_patterns = vec![
                "**/*.rst".to_string(),
                "**/*.md".to_string(),
                "**/*.txt".to_string(),
            ];
        }

        // Add built-in exclude patterns for common build artifacts and hidden files
        let mut all_exclude_patterns = exclude_patterns.clone();
        all_exclude_patterns.extend_from_slice(&[
            "_build/**".to_string(),
            "__pycache__/**".to_string(),
            ".git/**".to_string(),
            ".svn/**".to_string(),
            ".hg/**".to_string(),
            ".*/**".to_string(), // Skip all hidden directories
            "Thumbs.db".to_string(),
            ".DS_Store".to_string(),
        ]);

        // Exclude the actual output directory if it's inside the source directory
        // Canonicalize source (should always exist), but handle output specially
        let canonical_source = self.source_dir.canonicalize().unwrap_or_else(|_| self.source_dir.clone());

        // For output dir, try canonicalize, but if it doesn't exist yet, construct the path manually
        let canonical_output = self.output_dir.canonicalize().unwrap_or_else(|_| {
            // If output_dir is relative, join with source_dir
            if self.output_dir.is_relative() {
                canonical_source.join(&self.output_dir)
            } else {
                self.output_dir.clone()
            }
        });

        if let Ok(rel_output) = canonical_output.strip_prefix(&canonical_source) {
            let rel_output_str = rel_output.display().to_string();
            if !rel_output_str.is_empty() {
                let output_pattern = format!("{}/**", rel_output_str);
                debug!("Adding output directory exclusion pattern: {}", output_pattern);
                all_exclude_patterns.push(output_pattern);
                // Also add pattern without /** to exclude the directory itself
                all_exclude_patterns.push(rel_output_str);
            }
        } else {
            debug!(
                "Output directory {} is not inside source directory {}, no exclusion pattern added",
                canonical_output.display(),
                canonical_source.display()
            );
        }

        match matching::get_matching_files(
            &self.source_dir,
            &include_patterns,
            &all_exclude_patterns,
        ) {
            Ok(files) => Ok(files),
            Err(e) => {
                log::warn!(
                    "Pattern matching failed, falling back to simple discovery: {}",
                    e
                );
                // Fallback to old method if pattern matching fails
                let mut files = Vec::new();
                self.discover_files_sync(&self.source_dir, &mut files)?;
                Ok(files)
            }
        }
    }

    /// Fallback file discovery for when pattern matching fails
    fn discover_files_sync(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read directory: {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("Failed to read directory entry in: {}", dir.display()))?;
            let path = entry.path();

            if path.is_dir() {
                // Skip the output directory to avoid infinite loops
                // Use canonicalize to handle relative vs absolute paths
                let dominated_by_output = match (path.canonicalize(), self.output_dir.canonicalize()) {
                    (Ok(canonical_path), Ok(canonical_output)) => {
                        canonical_path == canonical_output || canonical_path.starts_with(&canonical_output)
                    }
                    _ => {
                        // Fallback to simple comparison if canonicalize fails
                        path == self.output_dir || path.starts_with(&self.output_dir)
                    }
                };
                if dominated_by_output {
                    continue;
                }

                // Skip hidden directories and build artifacts
                if let Some(name) = path.file_name() {
                    if name.to_string_lossy().starts_with('.')
                        || name == "_build"
                        || name == "__pycache__"
                    {
                        continue;
                    }
                }

                self.discover_files_sync(&path, files)?;
            } else if self.is_source_file(&path) {
                files.push(path);
            }
        }
        Ok(())
    }

    /// Fallback method to check if a file is a source file (used as backup)
    fn is_source_file(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            matches!(ext.to_string_lossy().as_ref(), "rst" | "md" | "txt")
        } else {
            false
        }
    }

    async fn build_dependency_graph(
        &self,
        files: &[PathBuf],
    ) -> Result<HashMap<PathBuf, Vec<PathBuf>>> {
        let mut graph = HashMap::new();

        // For now, simple implementation - process files in alphabetical order
        // TODO: Parse files to find actual dependencies (includes, references, etc.)
        for file in files {
            graph.insert(file.clone(), Vec::new());
        }

        Ok(graph)
    }

    async fn process_files_parallel(
        &self,
        files: &[PathBuf],
        _dependency_graph: &HashMap<PathBuf, Vec<PathBuf>>,
    ) -> Result<Vec<Document>> {
        info!(
            "Processing {} files with {} parallel jobs",
            files.len(),
            self.parallel_jobs
        );

        // Configure rayon thread pool
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.parallel_jobs)
            .build()?;

        let documents: Result<Vec<_>, _> = pool.install(|| {
            files
                .par_iter()
                .map(|file_path| self.process_single_file(file_path))
                .collect()
        });

        documents
    }

    fn process_single_file(&self, file_path: &Path) -> Result<Document> {
        // Safety check: refuse to process files inside the output directory
        if let (Ok(canonical_file), Ok(canonical_output)) =
            (file_path.canonicalize(), self.output_dir.canonicalize())
        {
            if canonical_file.starts_with(&canonical_output) {
                return Err(anyhow::anyhow!(
                    "Refusing to process file inside output directory: {}. \
                     Please delete the output directory and rebuild.",
                    file_path.display()
                ));
            }
        }

        let relative_path = file_path.strip_prefix(&self.source_dir).map_err(|_| {
            anyhow::anyhow!(
                "Path '{}' is not inside source directory '{}'. \
                 This can happen with symlinks or mixed absolute/relative paths.",
                file_path.display(),
                self.source_dir.display()
            )
        })?;
        debug!("Processing file: {}", relative_path.display());

        // Check cache if incremental build is enabled
        if self.incremental {
            if let Ok(cached_doc) = self.cache.get_document(file_path) {
                let file_mtime = utils::get_file_mtime(file_path)?;
                if cached_doc.source_mtime >= file_mtime {
                    debug!("Using cached version of {}", relative_path.display());
                    return Ok(cached_doc);
                }
            }
        }

        // Read and parse the file
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read source file: {}", file_path.display()))?;
        let document = self.parser.parse(file_path, &content)
            .with_context(|| format!("Failed to parse file: {}", file_path.display()))?;

        // Get the document path for navigation lookup
        let doc_path = relative_path
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");

        // Render document content to HTML with document titles and sections for toctree
        let mut renderer = HtmlRenderer::new();
        renderer.set_source_dir(self.source_dir.clone());
        {
            let titles = self.document_titles.lock().unwrap();
            for (path, title) in titles.iter() {
                renderer.register_document_title(path, title);
            }
        }
        {
            let sections = self.document_sections.lock().unwrap();
            for (path, section_list) in sections.iter() {
                renderer.register_document_sections(path, section_list.clone());
            }
        }
        let body_html = renderer.render_document_content(&document.content);

        // Get navigation context for this page
        let page_nav = {
            let nav = self.navigation.lock().unwrap();
            nav.get_page_navigation(&doc_path)
        };

        // Build the full HTML document using the template engine
        let rendered_html = self.render_full_html(&document, &body_html, &doc_path, &page_nav);

        // Write output file
        let output_path = self.get_output_path(file_path)?;
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
        }
        std::fs::write(&output_path, &rendered_html)
            .with_context(|| format!("Failed to write output file: {}", output_path.display()))?;

        // Cache the document
        if self.incremental {
            self.cache.store_document(file_path, &document)?;
        }

        Ok(document)
    }

    fn get_output_path(&self, source_path: &Path) -> Result<PathBuf> {
        let relative_path = source_path.strip_prefix(&self.source_dir).map_err(|_| {
            anyhow::anyhow!(
                "Path '{}' is not inside source directory '{}'. \
                 This can happen with symlinks or mixed absolute/relative paths.",
                source_path.display(),
                self.source_dir.display()
            )
        })?;
        let mut output_path = self.output_dir.join(relative_path);

        // Change extension to .html
        output_path.set_extension("html");

        Ok(output_path)
    }

    /// Render a full HTML document using the template engine
    fn render_full_html(
        &self,
        document: &Document,
        body_html: &str,
        doc_path: &str,
        page_nav: &PageNavigation,
    ) -> String {
        // Build CSS file list
        let mut css_files: Vec<String> = Vec::new();
        if let Some(ref theme) = self.active_theme {
            for stylesheet in &theme.stylesheets {
                if !stylesheet.path.is_empty() {
                    css_files.push(format!("_static/{}", stylesheet.path));
                }
            }
        }
        for css_file in &self.config.html_css_files {
            if !css_file.is_empty() {
                css_files.push(format!("_static/{}", css_file));
            }
        }

        // Build JS file list
        let mut script_files: Vec<String> = Vec::new();
        if let Some(ref theme) = self.active_theme {
            for script in &theme.scripts {
                if !script.path.is_empty() {
                    script_files.push(format!("_static/{}", script.path));
                }
            }
        }
        for js_file in &self.config.html_js_files {
            if !js_file.is_empty() {
                script_files.push(format!("_static/{}", js_file));
            }
        }

        // Get page title
        let title = if document.title.is_empty() || document.title == "Untitled" {
            String::new()
        } else {
            document.title.clone()
        };

        // Get master_doc (root_doc in config)
        let master_doc = self.config.root_doc.clone().unwrap_or_else(|| "index".to_string());

        // Render toctree for sidebar
        let toctree_html = {
            let nav = self.navigation.lock().unwrap();
            let mut options = ToctreeOptions::default();
            options.current_doc = Some(doc_path.to_string());
            nav.render_toctree(&options)
        };

        // Render page TOC from document's own TOC entries
        let page_toc_html = self.render_page_toc(document);
        let display_toc = document.toc.len() > 1;

        // Build template context
        let mut ctx = TemplateContext::new();

        // Core content
        ctx.insert("body", body_html).ok();
        ctx.insert("title", &title).ok();

        // Build docstitle in Sphinx format: "{project} {version} documentation"
        let docstitle = if let Some(ref version) = self.config.version {
            format!("{} {} documentation", self.config.project, version)
        } else {
            format!("{} documentation", self.config.project)
        };
        ctx.insert("docstitle", &docstitle).ok();
        ctx.insert("project", &self.config.project).ok();
        ctx.insert("version", &self.config.version).ok();

        // Language
        ctx.insert("language", self.config.language.as_deref().unwrap_or("en")).ok();

        // CSS and JS files
        ctx.insert("css_files", &css_files).ok();
        ctx.insert("script_files", &script_files).ok();

        // Navigation (with SafeHtml titles to avoid escaping rendered HTML)
        let parents_safe: Vec<NavLinkSafe> = page_nav.parents.iter().map(NavLinkSafe::from_nav_link).collect();
        let prev_safe = page_nav.prev.as_ref().map(NavLinkSafe::from_nav_link);
        let next_safe = page_nav.next.as_ref().map(NavLinkSafe::from_nav_link);
        ctx.insert("parents", &parents_safe).ok();
        ctx.insert("prev", &prev_safe).ok();
        ctx.insert("next", &next_safe).ok();
        ctx.insert("master_doc", &master_doc).ok();

        // Toctree for sidebar
        ctx.insert("toctree_html", &toctree_html).ok();

        // Page TOC
        ctx.insert("toc", &page_toc_html).ok();
        ctx.insert("display_toc", display_toc).ok();

        // Logo and favicon - use just the filename since we copy to _static
        if let Some(ref logo_path) = self.config.html_logo {
            if let Some(filename) = std::path::Path::new(logo_path).file_name() {
                if let Some(filename_str) = filename.to_str() {
                    ctx.insert("logo_url", filename_str).ok();
                    ctx.insert("logo_alt", "Logo").ok();
                }
            }
        }
        if let Some(ref favicon_path) = self.config.html_favicon {
            if let Some(filename) = std::path::Path::new(favicon_path).file_name() {
                if let Some(filename_str) = filename.to_str() {
                    ctx.insert("favicon_url", filename_str).ok();
                }
            }
        }

        // Copyright and attribution
        ctx.insert("copyright", self.config.copyright.as_deref().unwrap_or("")).ok();
        ctx.insert("show_copyright", self.config.copyright.is_some()).ok();
        ctx.insert("show_sphinx", true).ok();
        ctx.insert("sphinx_version", env!("CARGO_PKG_VERSION")).ok();

        // Source info
        ctx.insert("show_source", self.config.html_show_sourcelink.unwrap_or(true)).ok();
        ctx.insert("has_source", true).ok();
        let sourcename = format!("{}.rst.txt", doc_path);
        ctx.insert("sourcename", &sourcename).ok();

        // Theme options (with theme_ prefix for template access)
        // Use default values from the theme's options schema
        if let Some(ref theme) = self.active_theme {
            for (key, spec) in &theme.options_schema {
                let theme_key = format!("theme_{}", key);
                ctx.insert(&theme_key, &spec.default).ok();
            }
        }

        // Try to render using the template engine
        match self.template_engine.render("layout.html", &ctx.build()) {
            Ok(html) => html,
            Err(e) => {
                // Fallback to simple HTML if template fails
                warn!("Template rendering failed: {}, using fallback", e);
                self.render_fallback_html(document, body_html, &css_files, &script_files)
            }
        }
    }

    /// Render the page's own table of contents
    fn render_page_toc(&self, document: &Document) -> String {
        if document.toc.is_empty() {
            return String::new();
        }

        let renderer = crate::renderer::HtmlRenderer::new();
        let mut html = String::from("<ul>\n");
        for entry in &document.toc {
            // Render inline markup in the title (like `code` and :ref:)
            let rendered_title = renderer.render_rst_inline(&entry.title);
            html.push_str(&format!(
                "<li><a class=\"reference internal\" href=\"#{}\">{}</a></li>\n",
                html_escape::encode_text(&entry.anchor),
                rendered_title
            ));
        }
        html.push_str("</ul>\n");
        html
    }

    /// Fallback HTML rendering when template engine fails
    fn render_fallback_html(
        &self,
        document: &Document,
        body_html: &str,
        css_files: &[String],
        script_files: &[String],
    ) -> String {
        let page_title = if document.title.is_empty() || document.title == "Untitled" {
            self.config.project.clone()
        } else {
            format!("{} — {}", document.title, self.config.project)
        };

        let css_section: String = css_files
            .iter()
            .map(|f| format!(r#"<link rel="stylesheet" href="{}" />"#, f))
            .collect::<Vec<_>>()
            .join("\n    ");

        let js_section: String = script_files
            .iter()
            .map(|f| format!(r#"<script src="{}"></script>"#, f))
            .collect::<Vec<_>>()
            .join("\n    ");

        format!(
            r#"<!DOCTYPE html>
<html lang="{}">
<head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>{}</title>
    {}
</head>
<body>
    <div class="document">
        <div class="body">
            {}
        </div>
    </div>
    {}
</body>
</html>"#,
            self.config.language.as_deref().unwrap_or("en"),
            page_title,
            css_section,
            body_html,
            js_section
        )
    }

    async fn generate_indices(&self, _documents: &[Document]) -> Result<()> {
        info!("Generating indices and cross-references");
        // TODO: Implement index generation
        Ok(())
    }

    async fn copy_static_assets(&self) -> Result<()> {
        info!("Copying static assets");

        // Create _static directory
        let static_output_dir = self.output_dir.join("_static");
        tokio::fs::create_dir_all(&static_output_dir).await
            .with_context(|| format!("Failed to create static output directory: {}", static_output_dir.display()))?;

        // Copy theme static assets first (so project assets can override)
        if let Some(ref theme) = self.active_theme {
            if let Some(ref theme_static_dir) = theme.static_dir {
                if theme_static_dir.exists() {
                    info!("Copying theme static assets from {}", theme_static_dir.display());
                    self.copy_dir_to_static(theme_static_dir, &static_output_dir).await?;
                }
            }
        }

        // Copy built-in static assets - use relative path from binary location
        let exe_dir = std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Could not determine executable directory"))?
            .to_path_buf();

        // Try multiple possible locations for static assets
        let possible_static_dirs = [
            exe_dir.join("../static"),                      // Release build
            exe_dir.join("../../static"),                   // Debug build
            exe_dir.join("../../../static"),                // Deep build
            Path::new("rust-builder/static").to_path_buf(), // Local development
        ];

        let mut static_assets_copied = false;
        for builtin_static_dir in &possible_static_dirs {
            if builtin_static_dir.exists() {
                debug!("Found static assets at: {:?}", builtin_static_dir);
                for entry in std::fs::read_dir(builtin_static_dir)
                    .with_context(|| format!("Failed to read static directory: {}", builtin_static_dir.display()))?
                {
                    let entry = entry
                        .with_context(|| format!("Failed to read entry in static directory: {}", builtin_static_dir.display()))?;
                    let file_path = entry.path();
                    if file_path.is_file() {
                        let file_name = file_path.file_name().unwrap();
                        let dest_path = static_output_dir.join(file_name);
                        tokio::fs::copy(&file_path, &dest_path).await
                            .with_context(|| format!("Failed to copy static asset {} to {}", file_path.display(), dest_path.display()))?;
                        debug!("Copied static asset: {:?}", file_name);
                    }
                }
                static_assets_copied = true;
                break;
            }
        }

        if !static_assets_copied {
            debug!("No built-in static assets found, creating basic ones");
            // Create minimal CSS files if not found
            self.create_default_static_assets(&static_output_dir)
                .await?;
        }

        // Copy project-specific static assets from html_static_path (these override theme assets)
        for static_path in &self.config.html_static_path {
            let project_static = self.source_dir.join(static_path);
            if project_static.exists() {
                info!("Copying static assets from {}", project_static.display());
                self.copy_dir_to_static(&project_static, &static_output_dir).await?;
            } else {
                debug!("Static path does not exist: {}", project_static.display());
            }
        }

        // Copy logo to _static if specified (Sphinx behavior)
        if let Some(ref logo_path) = self.config.html_logo {
            let logo_src = self.source_dir.join(logo_path);
            if logo_src.exists() {
                let logo_filename = logo_src.file_name()
                    .ok_or_else(|| anyhow::anyhow!("Invalid logo path"))?;
                let logo_dest = static_output_dir.join(logo_filename);
                tokio::fs::copy(&logo_src, &logo_dest).await
                    .with_context(|| format!("Failed to copy logo from {} to {}", logo_src.display(), logo_dest.display()))?;
                info!("Copied logo to {}", logo_dest.display());
            }
        }

        // Copy favicon to _static if specified (Sphinx behavior)
        if let Some(ref favicon_path) = self.config.html_favicon {
            let favicon_src = self.source_dir.join(favicon_path);
            if favicon_src.exists() {
                let favicon_filename = favicon_src.file_name()
                    .ok_or_else(|| anyhow::anyhow!("Invalid favicon path"))?;
                let favicon_dest = static_output_dir.join(favicon_filename);
                tokio::fs::copy(&favicon_src, &favicon_dest).await
                    .with_context(|| format!("Failed to copy favicon from {} to {}", favicon_src.display(), favicon_dest.display()))?;
                info!("Copied favicon to {}", favicon_dest.display());
            }
        }

        Ok(())
    }

    /// Copy contents of a directory into the static output directory
    async fn copy_dir_to_static(&self, src_dir: &Path, dest_dir: &Path) -> Result<()> {
        utils::copy_dir_recursive(src_dir, dest_dir).await
    }

    /// Copy html_extra_path directories to the output root
    async fn copy_extra_paths(&self) -> Result<()> {
        if self.config.html_extra_path.is_empty() {
            return Ok(());
        }

        info!("Copying extra paths to output directory");

        // Pre-canonicalize source and output for safety checks
        let canonical_source = self.source_dir.canonicalize().ok();
        let canonical_output = self.output_dir.canonicalize().ok();

        for extra_path in &self.config.html_extra_path {
            // Resolve path relative to source directory
            let src_path = if extra_path.is_absolute() {
                extra_path.clone()
            } else {
                self.source_dir.join(extra_path)
            };

            if !src_path.exists() {
                warn!("html_extra_path '{}' does not exist, skipping", src_path.display());
                continue;
            }

            // Safety check: don't copy the source directory itself or the output directory
            if let Ok(canonical_src) = src_path.canonicalize() {
                if let Some(ref source) = canonical_source {
                    if &canonical_src == source || source.starts_with(&canonical_src) {
                        warn!(
                            "html_extra_path '{}' contains the source directory, skipping to prevent recursion",
                            src_path.display()
                        );
                        continue;
                    }
                }
                if let Some(ref output) = canonical_output {
                    if &canonical_src == output || canonical_src.starts_with(output) {
                        warn!(
                            "html_extra_path '{}' is inside the output directory, skipping",
                            src_path.display()
                        );
                        continue;
                    }
                }
            }

            if src_path.is_dir() {
                // Copy directory contents to output root, excluding output directory
                info!("Copying extra directory: {}", src_path.display());
                utils::copy_dir_recursive_excluding(&src_path, &self.output_dir, canonical_output.as_ref()).await
                    .with_context(|| format!(
                        "Failed to copy html_extra_path directory '{}' to '{}'",
                        src_path.display(),
                        self.output_dir.display()
                    ))?;
            } else if src_path.is_file() {
                // Copy single file to output root
                let file_name = src_path.file_name()
                    .ok_or_else(|| anyhow::anyhow!("Invalid file path: {}", src_path.display()))?;
                let dest_path = self.output_dir.join(file_name);
                info!("Copying extra file: {} -> {}", src_path.display(), dest_path.display());
                tokio::fs::copy(&src_path, &dest_path).await
                    .with_context(|| format!("Failed to copy extra file {} to {}", src_path.display(), dest_path.display()))?;
            }
        }

        Ok(())
    }

    async fn create_default_static_assets(&self, static_dir: &Path) -> Result<()> {
        // Create basic pygments.css
        let pygments_css = include_str!("../static/pygments.css");
        let path = static_dir.join("pygments.css");
        tokio::fs::write(&path, pygments_css).await
            .with_context(|| format!("Failed to write {}", path.display()))?;

        // Create basic theme.css
        let theme_css = include_str!("../static/theme.css");
        let path = static_dir.join("theme.css");
        tokio::fs::write(&path, theme_css).await
            .with_context(|| format!("Failed to write {}", path.display()))?;

        // Create basic JavaScript files
        let jquery_js = include_str!("../static/jquery.js");
        let path = static_dir.join("jquery.js");
        tokio::fs::write(&path, jquery_js).await
            .with_context(|| format!("Failed to write {}", path.display()))?;

        let doctools_js = include_str!("../static/doctools.js");
        let path = static_dir.join("doctools.js");
        tokio::fs::write(&path, doctools_js).await
            .with_context(|| format!("Failed to write {}", path.display()))?;

        let sphinx_highlight_js = include_str!("../static/sphinx_highlight.js");
        let path = static_dir.join("sphinx_highlight.js");
        tokio::fs::write(&path, sphinx_highlight_js).await
            .with_context(|| format!("Failed to write {}", path.display()))?;

        debug!("Created default static assets");
        Ok(())
    }

    async fn validate_documents(
        &self,
        processed_docs: &[Document],
        _source_files: &[PathBuf],
    ) -> Result<()> {
        info!("Validating documents and checking for warnings...");

        let mut toctree_references = HashSet::new();
        let mut referenced_files = HashSet::new();
        let mut all_documents = HashSet::new();

        // Collect all documents and their toctree references
        for doc in processed_docs {
            // Get relative path for comparison
            let doc_path_relative = doc
                .source_path
                .strip_prefix(&self.source_dir)
                .unwrap_or(&doc.source_path);
            let doc_path_no_ext = doc_path_relative.with_extension("");
            all_documents.insert(doc_path_no_ext.to_string_lossy().to_string());

            // Check for toctree directives and collect their references
            if let Some(toctree_refs) = self.extract_toctree_references(doc) {
                for toc_ref in toctree_refs {
                    toctree_references.insert((doc.source_path.clone(), toc_ref.clone()));
                    referenced_files.insert(toc_ref);
                }
            }
        }

        // Check for missing toctree references
        for (source_file, reference) in &toctree_references {
            // Skip external URLs and special references
            if reference.starts_with("http://")
                || reference.starts_with("https://")
                || reference.contains('<')  // External link syntax: "Title <url>"
                || reference.starts_with('@')  // Some external link conventions
            {
                continue;
            }

            let ref_path = format!("{}/index", reference);
            let alt_ref_path = reference.clone();

            if !all_documents.contains(&ref_path) && !all_documents.contains(&alt_ref_path) {
                let warning = BuildWarning::missing_toctree_ref(
                    source_file.clone(),
                    Some(10), // TODO: Extract actual line number
                    reference,
                );
                self.warnings.lock().unwrap().push(warning);
            }
        }

        // Check for orphaned documents
        for doc in processed_docs {
            let doc_path_relative = doc
                .source_path
                .strip_prefix(&self.source_dir)
                .unwrap_or(&doc.source_path);
            let doc_path_no_ext = doc_path_relative.with_extension("");
            let doc_path_str = doc_path_no_ext.to_string_lossy().to_string();

            // Skip the main index file
            if doc_path_str == "index" {
                continue;
            }

            // Check if this document is referenced in any toctree
            let is_referenced = referenced_files.iter().any(|ref_path| {
                ref_path == &doc_path_str
                    || ref_path == &format!("{}/index", doc_path_str)
                    || doc_path_str.starts_with(&format!("{}/", ref_path))
            });

            if !is_referenced {
                let warning = BuildWarning::orphaned_document(doc.source_path.clone());
                self.warnings.lock().unwrap().push(warning);
            }
        }

        let warning_count = self.warnings.lock().unwrap().len();
        info!("Validation completed. Found {} warnings", warning_count);

        Ok(())
    }

    fn extract_toctree_references(&self, doc: &Document) -> Option<Vec<String>> {
        use crate::document::DocumentContent;

        let mut references = Vec::new();

        if let DocumentContent::RestructuredText(rst_content) = &doc.content {
            for node in &rst_content.ast {
                if let crate::document::RstNode::Directive { name, content, .. } = node {
                    if name == "toctree" {
                        // Extract references from toctree content
                        for line in content.lines() {
                            let trimmed = line.trim();
                            if !trimmed.is_empty()
                                && !trimmed.starts_with(':')
                                && !trimmed.starts_with("..")
                            {
                                references.push(trimmed.to_string());
                            }
                        }
                    }
                }
            }
        }

        if references.is_empty() {
            None
        } else {
            Some(references)
        }
    }

    async fn generate_search_index(&self, _documents: &[Document]) -> Result<()> {
        info!("Generating search index");
        // TODO: Implement search index generation
        Ok(())
    }
}
