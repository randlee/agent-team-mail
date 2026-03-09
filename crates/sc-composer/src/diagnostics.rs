use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Non-fatal note produced during resolution/validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub path: Option<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub include_chain: Vec<PathBuf>,
}

impl Diagnostic {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
            line: None,
            column: None,
            include_chain: Vec::new(),
        }
    }

    pub fn with_path(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    pub fn with_position(mut self, line: usize, column: usize) -> Self {
        self.line = Some(line);
        self.column = Some(column);
        self
    }

    pub fn with_include_chain(mut self, include_chain: Vec<PathBuf>) -> Self {
        self.include_chain = include_chain;
        self
    }
}
