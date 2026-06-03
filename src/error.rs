use crate::lexer::Span;

/// Structured compiler error with optional source location and help text
#[derive(Debug, Clone)]
pub struct CompilerError {
    pub message: String,
    pub span: Option<Span>,
    pub help: Option<String>,
}

impl CompilerError {
    pub fn new(message: impl Into<String>) -> Self {
        CompilerError {
            message: message.into(),
            span: None,
            help: None,
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(span) = &self.span {
            write!(f, "Error at line {}, col {}: {}", span.line, span.col, self.message)?;
        } else {
            write!(f, "Error: {}", self.message)?;
        }
        if let Some(help) = &self.help {
            write!(f, "\n  help: {}", help)?;
        }
        Ok(())
    }
}

impl From<String> for CompilerError {
    fn from(s: String) -> Self {
        CompilerError::new(s)
    }
}

impl From<&str> for CompilerError {
    fn from(s: &str) -> Self {
        CompilerError::new(s.to_string())
    }
}
