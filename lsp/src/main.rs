use atomic::ast::*;
use atomic::error::CompilerError;
use atomic::lexer::Span;
use atomic::typecheck::TypeRegistry;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _};
use lsp_types::request::{GotoDefinition, HoverRequest, Request as _};
use lsp_types::*;
use std::collections::HashMap;

/// Symbol information collected from the AST for hover/go-to-def
#[derive(Debug, Clone)]
struct SymbolInfo {
    name: String,
    kind: SymbolKind,
    location: Span,
    detail: String, // type info, etc.
    body_span: Option<Span>, // span of the body (for go-to-def targeting)
}

#[derive(Debug, Clone)]
enum SymbolKind {
    Function,
    Variable,
    Constant,
    TypeAlias,
    Enum,
    Module,
}

/// Build a symbol table from the typed AST
fn build_symbol_index(stmts: &[Stmt], _registry: &TypeRegistry) -> Vec<SymbolInfo> {
    let mut symbols = Vec::new();

    for stmt in stmts {
        match stmt {
            Stmt::Fun { name, params, return_type, body: _, type_params, span, .. } => {
                let mut detail = String::from("fun ");
                if !type_params.is_empty() {
                    detail.push('<');
                    for (i, tp) in type_params.iter().enumerate() {
                        if i > 0 { detail.push_str(", "); }
                        detail.push_str(tp);
                    }
                    detail.push_str("> ");
                }
                detail.push_str(name);
                detail.push('(');
                for (i, p) in params.iter().enumerate() {
                    if i > 0 { detail.push_str(", "); }
                    detail.push_str(&p.name);
                    if let Some(ref ty) = p.ty {
                        detail.push_str(": ");
                        detail.push_str(&ty.to_string());
                    }
                }
                detail.push(')');
                if let Some(ref ret) = return_type {
                    detail.push_str(": ");
                    detail.push_str(&ret.to_string());
                }
                symbols.push(SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    location: *span,
                    detail,
                    body_span: Some(*span),
                });
            }
            Stmt::Let { name, type_ann, value: _, span, mutable, .. } => {
                let mut detail = if *mutable { String::from("var ") } else { String::from("val ") };
                detail.push_str(name);
                if let Some(ref ty) = type_ann {
                    detail.push_str(": ");
                    detail.push_str(&ty.to_string());
                }
                symbols.push(SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::Variable,
                    location: *span,
                    detail,
                    body_span: Some(*span),
                });
            }
            Stmt::Const { name, type_ann, value: _, span } => {
                let mut detail = String::from("const ");
                detail.push_str(name);
                if let Some(ref ty) = type_ann {
                    detail.push_str(": ");
                    detail.push_str(&ty.to_string());
                }
                symbols.push(SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::Constant,
                    location: *span,
                    detail,
                    body_span: Some(*span),
                });
            }
            Stmt::TypeAlias { name, type_params, definition, span } => {
                let mut detail = String::from("type ");
                if type_params.is_empty() {
                    detail.push_str(name);
                } else {
                    detail.push_str(&format!("{}[{}]", name, type_params.join(", ")));
                }
                detail.push_str(" = ");
                detail.push_str(&definition.to_string());
                symbols.push(SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::TypeAlias,
                    location: *span,
                    detail,
                    body_span: Some(*span),
                });
            }
            Stmt::Enum { name, type_params, variants: _, span } => {
                let mut detail = String::from("enum ");
                detail.push_str(name);
                if !type_params.is_empty() {
                    detail.push_str(&format!("[{}]", type_params.join(", ")));
                }
                symbols.push(SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::Enum,
                    location: *span,
                    detail,
                    body_span: Some(*span),
                });
            }
            Stmt::Module { name, body, span, .. } => {
                symbols.push(SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::Module,
                    location: *span,
                    detail: format!("module {}", name),
                    body_span: Some(*span),
                });
                // Recurse into module body
                let mut inner_symbols = build_symbol_index(body, _registry);
                symbols.append(&mut inner_symbols);
            }
            _ => {}
        }
    }

    symbols
}

/// Convert a CompilerError to an LSP Diagnostic
fn error_to_diagnostic(err: &CompilerError) -> Diagnostic {
    let range = err.span.map_or(
        Range { start: Position::new(0, 0), end: Position::new(0, 0) },
        |span| {
            let line = if span.line > 0 { span.line as u32 - 1 } else { 0 };
            let col = if span.col > 0 { span.col as u32 - 1 } else { 0 };
            Range {
                start: Position::new(line, col),
                end: Position::new(line, col + 1),
            }
        },
    );

    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        message: err.message.clone(),
        source: Some("atomic".to_string()),
        ..Diagnostic::default()
    }
}

/// Find the word at a given position in source
fn word_at_position(source: &str, pos: Position) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let line_idx = pos.line as usize;
    if line_idx >= lines.len() {
        return None;
    }
    let line = lines[line_idx];
    let col = pos.character as usize;

    // Find the word boundaries around col
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() || !chars[col].is_alphanumeric() && chars[col] != '_' {
        // Check if we're at a non-word char, try to find nearby word
        let mut start = col;
        let mut end = col;
        while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
            start -= 1;
        }
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        if start < end && start <= col && col <= end {
            return Some(chars[start..end].iter().collect());
        }
        return None;
    }

    let mut start = col;
    let mut end = col + 1;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }
    while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
        end += 1;
    }
    Some(chars[start..end].iter().collect())
}

/// Find a symbol by name (returns first match)
fn find_symbol<'a>(symbols: &'a [SymbolInfo], name: &str) -> Option<&'a SymbolInfo> {
    symbols.iter().find(|s| s.name == name)
}

/// Convert internal Span to LSP Range
fn span_to_range(span: Span) -> Range {
    let line = if span.line > 0 { span.line as u32 - 1 } else { 0 };
    let col = if span.col > 0 { span.col as u32 - 1 } else { 0 };
    Range {
        start: Position::new(line, col),
        end: Position::new(line, col + 1),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Atomic LSP server starting...");

    let (connection, io_threads) = Connection::stdio();
    let server_capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        ..Default::default()
    })?;

    let init_params = connection.initialize(server_capabilities)?;
    eprintln!("Atomic LSP initialized for: {:?}", init_params.get("rootUri"));

    // State
    let mut documents: HashMap<Url, String> = HashMap::new();

    // Main loop
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    break;
                }
                match req.method.as_str() {
                    HoverRequest::METHOD => {
                        handle_hover(&req, &documents, &connection);
                    }
                    GotoDefinition::METHOD => {
                        handle_goto_definition(&req, &documents, &connection);
                    }
                    _ => {
                        // Unknown request - ignore
                    }
                }
            }
            Message::Notification(not) => {
                match not.method.as_str() {
                    DidOpenTextDocument::METHOD => {
                        handle_did_open(&not, &mut documents, &connection);
                    }
                    DidChangeTextDocument::METHOD => {
                        handle_did_change(&not, &mut documents, &connection);
                    }
                    DidCloseTextDocument::METHOD => {
                        handle_did_close(&not, &mut documents);
                    }
                    _ => {}
                }
            }
            Message::Response(_resp) => {
                // We don't send requests, so ignore responses
            }
        }
    }

    io_threads.join()?;
    Ok(())
}

fn handle_did_open(
    not: &Notification,
    documents: &mut HashMap<Url, String>,
    connection: &Connection,
) {
    if let Ok(params) = serde_json::from_value::<DidOpenTextDocumentParams>(not.params.clone()) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        documents.insert(uri.clone(), text.clone());
        publish_diagnostics(&uri, &text, connection);
    }
}

fn handle_did_change(
    not: &Notification,
    documents: &mut HashMap<Url, String>,
    connection: &Connection,
) {
    if let Ok(params) = serde_json::from_value::<DidChangeTextDocumentParams>(not.params.clone()) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().last() {
            documents.insert(uri.clone(), change.text.clone());
            publish_diagnostics(&uri, &change.text, connection);
        }
    }
}

fn handle_did_close(not: &Notification, documents: &mut HashMap<Url, String>) {
    if let Ok(params) = serde_json::from_value::<DidCloseTextDocumentParams>(not.params.clone()) {
        documents.remove(&params.text_document.uri);
    }
}

fn publish_diagnostics(uri: &Url, source: &str, connection: &Connection) {
    let (stmts, registry, errors) = atomic::check_source(source);

    let diagnostics: Vec<Diagnostic> = errors.iter().map(error_to_diagnostic).collect();

    // Also collect parse warnings (from ariadne-style reporting — but for LSP we just use errors)
    // Store the AST and symbols for hover/go-to-def queries
    let _symbols = build_symbol_index(&stmts, &registry);

    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version: None,
    };

    let notification = Notification {
        method: "textDocument/publishDiagnostics".to_string(),
        params: serde_json::to_value(params).unwrap(),
    };
    connection.sender.send(Message::Notification(notification)).ok();
}

fn handle_hover(req: &Request, documents: &HashMap<Url, String>, connection: &Connection) {
    if let Ok(params) = serde_json::from_value::<HoverParams>(req.params.clone()) {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(source) = documents.get(&uri) {
            let (stmts, registry, _errors) = atomic::check_source(source);
            let symbols = build_symbol_index(&stmts, &registry);

            // Try to find the word at the cursor position
            if let Some(word) = word_at_position(source, pos) {
                // Look for a symbol with this name
                if let Some(sym) = find_symbol(&symbols, &word) {
                    let range = span_to_range(sym.location);
                    let hover = Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!("```atomic\n{}\n```", sym.detail),
                        }),
                        range: Some(range),
                    };
                    send_response(connection, req.id.clone(), hover);
                    return;
                }
            }
        }
    }
    // No hover info available
    send_null_response(connection, req.id.clone());
}

fn handle_goto_definition(req: &Request, documents: &HashMap<Url, String>, connection: &Connection) {
    if let Ok(params) = serde_json::from_value::<GotoDefinitionParams>(req.params.clone()) {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(source) = documents.get(&uri) {
            let (stmts, registry, _errors) = atomic::check_source(source);
            let symbols = build_symbol_index(&stmts, &registry);

            if let Some(word) = word_at_position(source, pos) {
                if let Some(sym) = find_symbol(&symbols, &word) {
                    let target_range = span_to_range(sym.location);
                    let goto = GotoDefinitionResponse::Scalar(Location {
                        uri: uri.clone(),
                        range: target_range,
                    });
                    send_response(connection, req.id.clone(), goto);
                    return;
                }
            }
        }
    }
    send_null_response(connection, req.id.clone());
}

fn send_response<T: serde::Serialize>(connection: &Connection, id: RequestId, result: T) {
    let resp = Response {
        id,
        result: Some(serde_json::to_value(result).unwrap()),
        error: None,
    };
    connection.sender.send(Message::Response(resp)).ok();
}

fn send_null_response(connection: &Connection, id: RequestId) {
    let resp = Response {
        id,
        result: Some(serde_json::Value::Null),
        error: None,
    };
    connection.sender.send(Message::Response(resp)).ok();
}
