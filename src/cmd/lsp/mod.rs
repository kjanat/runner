//! `runner lsp` — a Language Server for `runner.toml`.
//!
//! Speaks LSP over stdio and provides three editor features, each reusing the
//! same internals the CLI does:
//! - **diagnostics**: the exact `runner config validate` pipeline ([`crate::config`]
//!   + [`crate::resolver::validate_config`]), mapped to ranges;
//! - **hover**: section/field documentation pulled from the generated JSON
//!   Schema (i.e. the `RunnerConfig` doc comments);
//! - **completion**: section names, field names, and value sets (enums, booleans,
//!   and the runner/package-manager/source label vocabulary).
//!
//! The server is deliberately small and synchronous (one document at a time,
//! full-text sync) — runner.toml files are tiny, so there is no need for
//! incremental sync or a background analysis thread.

mod analysis;
mod diagnostics;
mod schema_index;
mod text;

use std::collections::HashMap;

use anyhow::Result;
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
};
use lsp_types::request::{Completion, HoverRequest, Request as _};
use lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, HoverParams, HoverProviderCapability,
    PublishDiagnosticsParams, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
    Uri,
};
use serde_json::Value;

use crate::config::CONFIG_FILENAME;

use self::schema_index::SchemaIndex;
use self::text::LineIndex;

/// Run the language server to completion over stdio. Returns the process exit
/// code (`0` on a clean shutdown).
pub(crate) fn run() -> Result<i32> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = serde_json::to_value(server_capabilities())?;
    let _initialize_params = connection.initialize(capabilities)?;

    let mut server = Server {
        documents: HashMap::new(),
        schema: SchemaIndex::build(),
    };
    server.serve(&connection)?;

    // Drop the connection before joining: the writer thread only terminates
    // once its channel sender (held by `connection`) is gone — otherwise
    // `join` blocks forever.
    drop(connection);
    io_threads.join()?;
    Ok(0)
}

/// The server's declared capabilities: full-text sync, hover, and completion
/// (triggered on the characters that begin a section, key value, or label).
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(
                ["[", "=", "\"", " ", ".", ","]
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            ),
            ..CompletionOptions::default()
        }),
        ..ServerCapabilities::default()
    }
}

/// Open-document store plus the cached schema documentation.
struct Server {
    /// Text of every open document, keyed by URI (full-sync, so always current).
    documents: HashMap<Uri, String>,
    /// Section/field documentation, built once at startup.
    schema: SchemaIndex,
}

impl Server {
    /// Main message loop. Returns when the client completes the shutdown/exit
    /// handshake (the receiver closes).
    fn serve(&mut self, connection: &Connection) -> Result<()> {
        for message in &connection.receiver {
            match message {
                Message::Request(request) => {
                    if connection.handle_shutdown(&request)? {
                        return Ok(());
                    }
                    self.handle_request(connection, request)?;
                }
                Message::Notification(notification) => {
                    self.handle_notification(connection, notification)?;
                }
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    /// Answer a hover/completion request; unknown methods get a null result so
    /// the client isn't left waiting.
    fn handle_request(&self, connection: &Connection, request: Request) -> Result<()> {
        let method = request.method.clone();
        let response = match method.as_str() {
            HoverRequest::METHOD => {
                let (id, params) = request.extract::<HoverParams>(HoverRequest::METHOD)?;
                Response::new_ok(id, self.hover(&params))
            }
            Completion::METHOD => {
                let (id, params) = request.extract::<CompletionParams>(Completion::METHOD)?;
                Response::new_ok(id, CompletionResponse::Array(self.completion(&params)))
            }
            _ => Response::new_ok(request.id, Value::Null),
        };
        connection.sender.send(Message::Response(response))?;
        Ok(())
    }

    /// Apply a document-sync notification and (re)publish diagnostics.
    fn handle_notification(
        &mut self,
        connection: &Connection,
        notification: Notification,
    ) -> Result<()> {
        let method = notification.method.clone();
        match method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params = notification
                    .extract::<lsp_types::DidOpenTextDocumentParams>(DidOpenTextDocument::METHOD)?;
                let uri = params.text_document.uri;
                self.documents
                    .insert(uri.clone(), params.text_document.text);
                self.publish_diagnostics(connection, &uri);
            }
            DidChangeTextDocument::METHOD => {
                let mut params = notification.extract::<lsp_types::DidChangeTextDocumentParams>(
                    DidChangeTextDocument::METHOD,
                )?;
                // Full sync: the last change carries the whole document.
                if let Some(change) = params.content_changes.pop() {
                    let uri = params.text_document.uri;
                    self.documents.insert(uri.clone(), change.text);
                    self.publish_diagnostics(connection, &uri);
                }
            }
            DidCloseTextDocument::METHOD => {
                let params = notification.extract::<lsp_types::DidCloseTextDocumentParams>(
                    DidCloseTextDocument::METHOD,
                )?;
                let uri = params.text_document.uri;
                self.documents.remove(&uri);
                // Clear diagnostics for the closed file.
                send_diagnostics(connection, uri, Vec::new());
            }
            _ => {}
        }
        Ok(())
    }

    fn publish_diagnostics(&self, connection: &Connection, uri: &Uri) {
        let diagnostics = self
            .documents
            .get(uri)
            .filter(|_| is_runner_toml(uri))
            .map(|text| diagnostics::compute(text, &LineIndex::new(text)))
            .unwrap_or_default();
        send_diagnostics(connection, uri.clone(), diagnostics);
    }

    fn hover(&self, params: &HoverParams) -> Option<lsp_types::Hover> {
        let pos = params.text_document_position_params.position;
        let uri = &params.text_document_position_params.text_document.uri;
        let text = self.documents.get(uri).filter(|_| is_runner_toml(uri))?;
        analysis::hover(&LineIndex::new(text), &self.schema, text, pos)
    }

    fn completion(&self, params: &CompletionParams) -> Vec<lsp_types::CompletionItem> {
        let pos = params.text_document_position.position;
        let uri = &params.text_document_position.text_document.uri;
        let Some(text) = self.documents.get(uri).filter(|_| is_runner_toml(uri)) else {
            return Vec::new();
        };
        analysis::completion(
            &LineIndex::new(text),
            &self.schema,
            text,
            pos,
            document_dir(uri).as_deref(),
        )
    }
}

/// The document's directory for a `file:` URI, anchoring project-task
/// discovery. Non-file URIs (or a rootless path) yield `None`.
fn document_dir(uri: &Uri) -> Option<std::path::PathBuf> {
    if uri.scheme().is_some_and(|s| s.as_str() != "file") {
        return None;
    }
    let path = std::path::Path::new(uri.path().as_str());
    path.parent()
        .filter(|parent| parent.is_absolute())
        .map(std::path::Path::to_path_buf)
}

/// Whether `uri`'s basename is `runner.toml` or its dotfile form, at any depth —
/// each directory can hold its own config.
fn is_runner_toml(uri: &Uri) -> bool {
    uri.path().as_str().rsplit('/').next().is_some_and(|name| {
        name == CONFIG_FILENAME || name.strip_prefix('.') == Some(CONFIG_FILENAME)
    })
}

/// Send a `publishDiagnostics` notification for `uri`.
fn send_diagnostics(connection: &Connection, uri: Uri, diagnostics: Vec<lsp_types::Diagnostic>) {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    if let Ok(params) = serde_json::to_value(params) {
        let _ = connection.sender.send(Message::Notification(Notification {
            method: lsp_types::notification::PublishDiagnostics::METHOD.to_string(),
            params,
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lsp_types::Uri;

    use super::document_dir;

    #[test]
    fn document_dir_takes_the_file_uri_parent() {
        let uri = Uri::from_str("file:///home/user/proj/runner.toml").expect("uri");
        assert_eq!(
            document_dir(&uri).as_deref(),
            Some(std::path::Path::new("/home/user/proj"))
        );
    }

    #[test]
    fn document_dir_rejects_non_file_schemes() {
        let uri = Uri::from_str("untitled:Untitled-1").expect("uri");
        assert_eq!(document_dir(&uri), None);
    }
}
