use std::path::PathBuf;

use sourcemap::SourceMap;
use swc_common::{sync::Lrc, SourceMap as SwcSourceMap};
use swc_ecma_ast::{ImportDecl, Module};
use swc_ecma_visit::{Node, Visit, VisitWith};

use super::{config::V8_SUPPORTED_JS_FEATURES, Lintable, Parseable, Validate};

// bring implemntations of Lintable and Parseable into scope
mod lint;
mod parse;

/// A representation of a JS file (+ an optional source map)
/// produced by a bundler's output
pub struct JavaScript {
    module: Module,
    swc_source_map: Lrc<SwcSourceMap>,
    js_source_map: Option<SourceMap>,
}

impl Validate<PathBuf> for JavaScript {}

impl std::fmt::Debug for JavaScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JavaScript")
            .field("module", &self.module)
            .field("js_source_map", &self.js_source_map)
            // .finish_non_exhaustive()
            .finish()
    }
}

impl JavaScript {
    /// Find any other files imported by this JS file
    pub fn find_imports(&self) -> Vec<String> {
        let mut import_finder = ImportFinder { imports: vec![] };
        self.module.visit_children_with(&mut import_finder);
        import_finder.imports
    }
}

#[doc = "hidden"]
struct ImportFinder {
    pub imports: Vec<String>,
}

impl Visit for ImportFinder {
    fn visit_import_decl(&mut self, n: &ImportDecl, _parent: &dyn Node) {
        self.imports.push(n.src.value.to_string());
    }
}
