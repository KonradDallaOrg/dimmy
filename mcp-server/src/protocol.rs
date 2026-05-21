//! JSON-RPC 2.0 request/response types over MCP framing.
//!
//! Frame format: one JSON object per line on stdin/stdout, no
//! Content-Length header (Claude Desktop supports both the LSP-style
//! framed protocol and the line-delimited variant; we use the
//! latter because it's simpler and matches the MCP TypeScript SDK
//! default).
//!
//! Error codes follow the JSON-RPC 2.0 spec:
//!   -32700 Parse error           (malformed JSON)
//!   -32600 Invalid request       (missing required fields)
//!   -32601 Method not found
//!   -32602 Invalid params
//!   -32603 Internal error
//!   -32002 Server not initialized (MCP-specific)

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Request {
    #[allow(dead_code)] // present in every JSON-RPC frame; spec-mandated
    pub jsonrpc: String,
    /// None means notification (no response expected).
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

impl Response {
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: id.unwrap_or(serde_json::Value::Null),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<serde_json::Value>, error: Error) -> Self {
        Self {
            jsonrpc: "2.0",
            id: id.unwrap_or(serde_json::Value::Null),
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Error {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {}", method),
            data: None,
        }
    }

    pub fn not_initialized() -> Self {
        Self {
            code: -32002,
            message: "Server not initialized. Send `initialize` first.".to_string(),
            data: None,
        }
    }

    pub fn invalid_params(reason: &str) -> Self {
        Self {
            code: -32602,
            message: format!("Invalid params: {}", reason),
            data: None,
        }
    }

    pub fn internal(reason: &str) -> Self {
        Self {
            code: -32603,
            message: format!("Internal error: {}", reason),
            data: None,
        }
    }
}
