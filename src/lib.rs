// Library API for the Atomic language — used by both the CLI and LSP server.
// Does NOT include codegen (which depends on LLVM) to keep the LSP lightweight.

pub mod ast;
pub mod config;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod typecheck;

use ast::*;
use error::CompilerError;
use std::fs;
use std::path::{Path, PathBuf};
use typecheck::{TypeChecker, TypeRegistry};

/// Lex, parse, and type-check a source string.
/// Returns the AST, type registry, and any errors found.
pub fn check_source(source: &str) -> (Vec<Stmt>, TypeRegistry, Vec<CompilerError>) {
    let mut errors: Vec<CompilerError> = Vec::new();

    let mut lexer = lexer::Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = parser::Parser::new(tokens);
    let program = match parser.parse_program() {
        Ok(p) => p,
        Err(e) => {
            errors.push(CompilerError::new(e.message)
                .with_span(lexer::Span::new(0, e.line, e.col)));
            return (vec![], TypeRegistry::new(), errors);
        }
    };

    let mut registry = TypeRegistry::new();
    for stmt in &program.stmts {
        if let Err(e) = registry.register(stmt) {
            errors.push(CompilerError::new(e));
        }
    }

    let mut checker = TypeChecker::new(registry.clone());
    let mut check_errors = checker.check(&program);
    errors.append(&mut check_errors);

    (program.stmts, registry, errors)
}

/// Resolve a module file path from an import string.
pub fn resolve_module(base: &Path, module_name: &str) -> Option<PathBuf> {
    let candidates = [
        base.join(format!("{}.at", module_name)),
        base.join(format!("{}.atom", module_name)),
        base.join(format!("{}/mod.at", module_name)),
        base.join(format!("{}/mod.atom", module_name)),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }
    None
}

/// Read, parse, and type-check a file.
pub fn check_file(file_path: &Path) -> Result<(Vec<Stmt>, TypeRegistry), Vec<CompilerError>> {
    let source = fs::read_to_string(file_path)
        .map_err(|e| vec![CompilerError::new(format!("Cannot read file: {}", e))])?;
    let (stmts, registry, errors) = check_source(&source);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok((stmts, registry))
}
