//! Source coverage is checked by the dev command before compiling the engine.
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use syn::{
    Attribute,
    spanned::Spanned,
    visit::{self, Visit},
};

#[derive(Default, Serialize)]
pub struct Coverage {
    pub files: usize,
    pub functions: usize,
    pub interface_methods: usize,
    pub call_expressions: usize,
    pub constant_functions: Vec<String>,
    pub missing: Vec<String>,
    pub macros: Vec<String>,
}

fn marked(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attr| {
        let syn::Meta::List(list) = &attr.meta else {
            return false;
        };
        attr.path().is_ident("cfg_attr")
            && list.tokens.to_string() == "feature = \"dev\" , duckdb_dev :: instrument"
    })
}

struct Inspect<'a> {
    path: &'a Path,
    report: &'a mut Coverage,
    additions: Vec<usize>,
    covered: bool,
}

impl Inspect<'_> {
    fn require(
        &mut self,
        attributes: &[Attribute],
        span: proc_macro2::Span,
        name: impl std::fmt::Display,
    ) -> bool {
        let covered = self.covered || marked(attributes);
        if !covered {
            self.report.missing.push(format!(
                "{}:{} {name}",
                self.path.display(),
                span.start().line
            ));
            self.additions.push(span.start().line);
        }
        covered
    }

    fn function(&mut self, signature: &syn::Signature) {
        if signature.constness.is_some() {
            self.report.constant_functions.push(format!(
                "{}:{} {}",
                self.path.display(),
                signature.span().start().line,
                signature.ident
            ));
        } else {
            self.report.functions += 1;
        }
    }
}

impl<'ast> Visit<'ast> for Inspect<'_> {
    fn visit_expr_method_call(&mut self, item: &'ast syn::ExprMethodCall) {
        self.report.call_expressions += 1;
        visit::visit_expr_method_call(self, item);
    }
    fn visit_expr_call(&mut self, item: &'ast syn::ExprCall) {
        if !matches!(item.func.as_ref(), syn::Expr::Path(path) if path.path.segments.last().is_some_and(|part|
            part.ident.to_string().chars().next().is_some_and(char::is_uppercase)))
        {
            self.report.call_expressions += 1;
        }
        visit::visit_expr_call(self, item);
    }
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.function(&item.sig);
        let before = self.covered;
        self.covered = self.require(&item.attrs, item.span(), &item.sig.ident) || !before;
        visit::visit_item_fn(self, item);
        self.covered = before;
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let before = self.covered;
        self.covered = self.require(&item.attrs, item.span(), "impl") || !before;
        visit::visit_item_impl(self, item);
        self.covered = before;
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.function(&item.sig);
        visit::visit_impl_item_fn(self, item);
    }
    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        let before = self.covered;
        self.covered = self.require(&item.attrs, item.span(), &item.ident) || !before;
        visit::visit_item_trait(self, item);
        self.covered = before;
    }
    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        self.report.interface_methods += 1;
        if item.default.is_some() {
            self.function(&item.sig);
        }
        visit::visit_trait_item_fn(self, item);
    }
    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if item.mac.path.is_ident("macro_rules") {
            let entry = format!(
                "{}:{} {}",
                self.path.display(),
                item.span().start().line,
                item.ident
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            );
            self.report.macros.push(entry.clone());
            if !item
                .mac
                .tokens
                .to_string()
                .contains("duckdb_dev :: instrument")
            {
                self.report.missing.push(format!(
                    "{entry}: review and instrument generated functions"
                ));
            }
        }
    }
}

fn files(path: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            files(&path, found)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
    Ok(())
}

/// Applies only mechanical cfg attributes. README and unrelated files are never read or written.
pub fn audit(root: &Path, write: bool) -> io::Result<Coverage> {
    let mut paths = Vec::new();
    for directory in ["src", "tools", "test", "benchmark"] {
        files(&root.join(directory), &mut paths)?;
    }
    paths.sort();
    let mut report = Coverage::default();
    for path in paths {
        let source = fs::read_to_string(&path)?;
        let ast = syn::parse_file(&source)
            .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?;
        report.files += 1;
        let mut inspect = Inspect {
            path: path.strip_prefix(root).unwrap_or(&path),
            report: &mut report,
            additions: Vec::new(),
            covered: false,
        };
        inspect.visit_file(&ast);
        if write && !inspect.additions.is_empty() {
            inspect.additions.sort_unstable();
            inspect.additions.dedup();
            let mut output = String::with_capacity(source.len() + inspect.additions.len() * 64);
            for (line, text) in source.split_inclusive('\n').enumerate() {
                if inspect.additions.binary_search(&(line + 1)).is_ok() {
                    let indentation = text.len() - text.trim_start().len();
                    output.push_str(&text[..indentation]);
                    output.push_str("#[cfg_attr(feature = \"dev\", duckdb_dev::instrument)]\n");
                }
                output.push_str(text);
            }
            fs::write(&path, output)?;
        }
    }
    Ok(report)
}
