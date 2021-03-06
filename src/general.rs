use context::*;
use languageserver_types::notification::Notification;
use languageserver_types::request::Request;
use languageserver_types::*;
use serde_json::Value;
use std::process;
use toml;
use types::*;
use url::Url;
use util::*;

pub fn initialize(root_path: &str, options: Option<Value>, meta: &EditorMeta, ctx: &mut Context) {
    let params = InitializeParams {
        capabilities: ClientCapabilities {
            workspace: Some(WorkspaceClientCapabilities {
                workspace_edit: Some(WorkspaceEditCapability {
                    document_changes: Some(true),
                    resource_operations: Some(vec![
                        ResourceOperationKind::Create,
                        ResourceOperationKind::Delete,
                        ResourceOperationKind::Rename,
                    ]),
                    ..WorkspaceEditCapability::default()
                }),
                ..WorkspaceClientCapabilities::default()
            }),
            text_document: Some(TextDocumentClientCapabilities {
                completion: Some(CompletionCapability {
                    completion_item: Some(CompletionItemCapability {
                        documentation_format: Some(vec![MarkupKind::PlainText]),
                        ..CompletionItemCapability::default()
                    }),
                    ..CompletionCapability::default()
                }),
                ..TextDocumentClientCapabilities::default()
            }),
            experimental: None,
        },
        initialization_options: options,
        process_id: Some(process::id().into()),
        root_uri: Some(Url::from_file_path(root_path).unwrap()),
        root_path: None,
        trace: Some(TraceOption::Off),
        workspace_folders: None,
    };

    let id = ctx.next_request_id();
    ctx.response_waitlist.insert(
        id.clone(),
        (
            meta.clone(),
            request::Initialize::METHOD.into(),
            toml::Value::Table(toml::value::Table::default()),
        ),
    );
    ctx.call(id, request::Initialize::METHOD.into(), params);
}

pub fn exit(ctx: &mut Context) {
    // NOTE we can't use Params::None because it's serialized as Value::Array([])
    let params: Option<u8> = None;
    ctx.notify(notification::Exit::METHOD.into(), params);
}

pub fn capabilities(meta: &EditorMeta, ctx: &mut Context) {
    // NOTE controller should park request for capabilities until they are available thus it should
    // be safe to unwrap here (otherwise something unexpectedly wrong and it's better to panic)

    let server_capabilities = ctx.capabilities.as_ref().unwrap();

    let mut features = vec![];

    if server_capabilities.hover_provider.unwrap_or(false) {
        features.push("lsp-hover");
    }

    if server_capabilities.completion_provider.is_some() {
        features.push("lsp-completion (hooked on InsertIdle)");
    }

    if server_capabilities.definition_provider.unwrap_or(false) {
        features.push("lsp-definition (mapped to `gd` by default)");
    }

    if server_capabilities.references_provider.unwrap_or(false) {
        features.push("lsp-references");
    }

    if server_capabilities
        .workspace_symbol_provider
        .unwrap_or(false)
    {
        features.push("lsp-workspace-symbol");
    }

    if server_capabilities
        .document_formatting_provider
        .unwrap_or(false)
    {
        features.push("lsp-formatting");
    }

    if let Some(ref rename_provider) = server_capabilities.rename_provider {
        match rename_provider {
            RenameProviderCapability::Simple(true) | RenameProviderCapability::Options(_) => {
                features.push("lsp-rename")
            }
            _ => (),
        }
    }

    features.push("lsp-diagnostics");

    let command = format!(
        "info 'kak-lsp commands supported by {} language server:\n\n{}'",
        ctx.language_id,
        editor_escape(&features.join("\n"))
    );
    ctx.exec(meta.clone(), command);
}
