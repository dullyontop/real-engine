//! The rmcp server handler: tools from the command registry, calls through
//! the engine handle.

use std::borrow::Cow;
use std::path::Path;
use std::sync::{Arc, Mutex};

use re_command::{CommandOutcome, CommandSpec, EngineHandle, Tier};
use re_core::Diagnostic;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value as Json, json};

use crate::{McpOptions, codes};

/// The longest tool description sent (clients truncate at 4096 characters).
pub const MAX_DESCRIPTION: usize = 4000;

/// The MCP tool name of a command (`entity.spawn` → `entity_spawn`).
pub fn tool_name(command: &str) -> String {
    command.replace('.', "_")
}

/// The command a tool name stands for, among `specs`.
pub fn command_name<'a>(tool: &str, specs: &'a [CommandSpec]) -> Option<&'a str> {
    specs
        .iter()
        .map(|s| s.name.as_str())
        .find(|name| tool_name(name) == tool)
}

/// Makes a command's input schema acceptable to every client: the root is
/// an object with `type: "object"` and no `anyOf`/`oneOf`/`allOf`, and no
/// `format` or `$schema` keywords remain anywhere.
pub fn sanitize_schema(schema: &Json) -> JsonObject {
    // `format` is removed as a keyword, not as a property called "format".
    let mut root = match sanitize_schema_value(schema) {
        Json::Object(map) => map,
        _ => JsonObject::new(),
    };
    for key in ["anyOf", "oneOf", "allOf"] {
        root.remove(key);
    }
    root.insert("type".into(), Json::from("object"));
    root.entry("properties")
        .or_insert_with(|| Json::Object(JsonObject::new()));
    root
}

fn description(spec: &CommandSpec) -> String {
    let tier = match spec.tier {
        Tier::Read => "Read-only.".to_string(),
        Tier::Edit => {
            "Changes the project (edit tier; undoable). Pass dry_run: true to preview the changes without making them.".to_string()
        }
        Tier::Destructive => {
            "Can lose work (destructive tier). Pass dry_run: true to preview without making the change.".to_string()
        }
    };
    let mut text = format!(
        "{} {tier} Engine command `{}`.",
        spec.summary.trim(),
        spec.name
    );
    if text.len() > MAX_DESCRIPTION {
        let mut cut = MAX_DESCRIPTION;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

fn title(spec: &CommandSpec) -> String {
    let mut words = spec.name.replace(['.', '_'], " ");
    if let Some(first) = words.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    words
}

/// The output schema of a command's tool: the outcome envelope
/// `{ ok, changes, diagnostics, data }`, with `data` described by the
/// command's [`CommandSpec::output_schema`] when it has one (its `$defs` are
/// moved to the root, so its references still resolve) and open otherwise.
pub fn output_schema(spec: &CommandSpec) -> JsonObject {
    let mut defs = JsonObject::new();
    let data = match &spec.output_schema {
        Some(schema) => {
            let mut data = sanitize_schema_value(schema);
            if let Some(map) = data.as_object_mut() {
                // schemars (2020-12) refers to `#/$defs/<name>`.
                if let Some(Json::Object(d)) = map.remove("$defs") {
                    defs = d;
                }
                map.remove("title");
            }
            data
        }
        None => json!({ "description": "The command's result data." }),
    };
    let mut root = match json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean", "description": "True: the command succeeded." },
            "changes": {
                "type": "array",
                "description": "The operations applied (for a dry run, those that would be).",
                "items": { "type": "object" }
            },
            "diagnostics": {
                "type": "array",
                "description": "Warnings and notes (errors come back as isError results).",
                "items": {
                    "type": "object",
                    "properties": {
                        "code": { "type": "string" },
                        "severity": { "type": "string" },
                        "message": { "type": "string" }
                    },
                    "required": ["code", "severity", "message"]
                }
            },
            "data": data
        },
        "required": ["ok", "changes", "diagnostics", "data"]
    }) {
        Json::Object(map) => map,
        _ => unreachable!("a JSON object literal"),
    };
    if !defs.is_empty() {
        root.insert("$defs".into(), Json::Object(defs));
    }
    root
}

/// [`sanitize_schema`]'s keyword cleanup without forcing an object root.
fn sanitize_schema_value(schema: &Json) -> Json {
    fn strip(value: &mut Json) {
        match value {
            Json::Object(map) => {
                if map.get("format").is_some_and(Json::is_string) {
                    map.remove("format");
                }
                map.remove("$schema");
                for v in map.values_mut() {
                    strip(v);
                }
            }
            Json::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value = schema.clone();
    strip(&mut value);
    value
}

/// The MCP tool of a command.
pub fn tool_for(spec: &CommandSpec) -> Tool {
    let a = spec.annotations;
    let mut tool = Tool::new(
        tool_name(&spec.name),
        description(spec),
        Arc::new(sanitize_schema(&spec.input_schema)),
    );
    tool.output_schema = Some(Arc::new(output_schema(spec)));
    tool.with_title(title(spec))
        .with_annotations(ToolAnnotations::from_raw(
            Some(title(spec)),
            Some(a.read_only_hint),
            Some(a.destructive_hint),
            Some(a.idempotent_hint),
            Some(a.open_world_hint),
        ))
}

/// The JSON text of a failed outcome (ADR-0026): the first error's `code`,
/// `message`, `location`, `hint` and `explain`, plus any further
/// diagnostics.
pub fn error_json(outcome: &CommandOutcome) -> Json {
    let first = outcome
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .or(outcome.diagnostics.first());
    let Some(first) = first else {
        return json!({ "code": null, "message": "the command failed without a diagnostic" });
    };
    let mut body = serde_json::to_value(first).unwrap_or(Json::Null);
    if let Some(map) = body.as_object_mut() {
        map.insert(
            "explain".into(),
            Json::from(format!("real explain {}", first.code)),
        );
        let others: Vec<&Diagnostic> = outcome
            .diagnostics
            .iter()
            .filter(|d| !std::ptr::eq(*d, first))
            .collect();
        if !others.is_empty() {
            map.insert(
                "diagnostics".into(),
                serde_json::to_value(others).unwrap_or(Json::Null),
            );
        }
    }
    body
}

/// Paths of PNG files named by string values at the top level of `data`.
/// Only relative paths that stay inside the project count, so a result can
/// never make the server read a file from elsewhere.
fn png_paths(data: &Json) -> Vec<String> {
    data.as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(Json::as_str)
        .filter(|s| s.to_ascii_lowercase().ends_with(".png"))
        .filter_map(|s| re_agent::policy::normalize(s).ok())
        .collect()
}

/// Standard base64 (RFC 4648) with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn result_for(outcome: CommandOutcome, project: &Path) -> CallToolResult {
    if !outcome.ok {
        return CallToolResult::error(vec![ContentBlock::text(error_json(&outcome).to_string())]);
    }
    let mut outcome = outcome;
    let mut images = Vec::new();
    for rel in png_paths(&outcome.data) {
        let path = project.join(&rel);
        match std::fs::read(&path) {
            Ok(bytes) => images.push(ContentBlock::image(base64(&bytes), "image/png")),
            Err(e) => outcome.diagnostics.push(
                Diagnostic::warning(
                    codes::IMAGE_UNREADABLE,
                    format!("could not attach the image {rel}: {e}"),
                )
                .with_hint("the path is still in the result; open the file directly"),
            ),
        }
    }
    let value = serde_json::to_value(&outcome).unwrap_or(Json::Null);
    let mut result = CallToolResult::structured(value);
    result.content.extend(images);
    result
}

/// The MCP server over one engine handle.
#[derive(Clone)]
pub struct RealEngineServer {
    handle: EngineHandle,
    options: Arc<McpOptions>,
    /// The specs last listed (refreshed by `tools/list`, and on a miss).
    specs: Arc<Mutex<Option<Vec<CommandSpec>>>>,
    /// Calls reach the engine one at a time. Requests are handled on separate
    /// tasks, so calls sent together may still run in either order; clients
    /// wait for a result before a call that depends on it.
    order: Arc<tokio::sync::Mutex<()>>,
}

impl RealEngineServer {
    /// A server answering through `handle`.
    pub fn new(handle: EngineHandle, options: McpOptions) -> RealEngineServer {
        RealEngineServer {
            handle,
            options: Arc::new(options),
            specs: Arc::new(Mutex::new(None)),
            order: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    async fn fetch_specs(&self) -> Result<Vec<CommandSpec>, McpError> {
        let handle = self.handle.clone();
        let specs = tokio::task::spawn_blocking(move || handle.list_specs())
            .await
            .map_err(|e| McpError::internal_error(format!("engine call failed: {e}"), None))?
            .map_err(|d| {
                McpError::internal_error(
                    d.message.clone(),
                    Some(json!({ "code": d.code.to_string() })),
                )
            })?;
        *self.specs.lock().unwrap_or_else(|p| p.into_inner()) = Some(specs.clone());
        Ok(specs)
    }

    async fn command_for(&self, tool: &str) -> Result<Option<String>, McpError> {
        let cached = self.specs.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(specs) = &cached
            && let Some(name) = command_name(tool, specs)
        {
            return Ok(Some(name.to_string()));
        }
        let specs = self.fetch_specs().await?;
        Ok(command_name(tool, &specs).map(str::to_string))
    }
}

impl ServerHandler for RealEngineServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            self.options.name.clone(),
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(self.options.instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let specs = self.fetch_specs().await?;
        Ok(ListToolsResult::with_all_items(
            specs.iter().map(tool_for).collect(),
        ))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = self
            .options
            .resources
            .list()
            .iter()
            .map(|r| {
                Resource::new(r.uri.clone(), r.name.clone())
                    .with_description(r.description.clone())
                    .with_mime_type(r.mime_type.clone())
                    .with_size(r.text.len() as u64)
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        match self.options.resources.read(&request.uri) {
            Ok(r) => Ok(
                ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
                    uri: r.uri.clone(),
                    mime_type: Some(r.mime_type.clone()),
                    text: r.text.to_string(),
                    meta: None,
                }])
                .into(),
            ),
            // MCP's "resource not found" error code.
            Err(d) => Err(McpError::new(
                rmcp::model::ErrorCode(-32002),
                d.message.clone(),
                Some(json!({ "uri": request.uri, "code": d.code.to_string() })),
            )),
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let _turn = self.order.lock().await;
        let Some(command) = self.command_for(&request.name).await? else {
            return Err(McpError::invalid_params(
                Cow::Owned(format!("tool not found: {}", request.name)),
                None,
            ));
        };
        let args = Json::Object(request.arguments.unwrap_or_default());
        let handle = self.handle.clone();
        let outcome = tokio::task::spawn_blocking(move || handle.call(&command, args, false))
            .await
            .map_err(|e| McpError::internal_error(format!("engine call failed: {e}"), None))?;
        Ok(result_for(outcome, &self.options.project).into())
    }
}

/// Serves `server` on `transport` until the client disconnects.
pub(crate) async fn serve<T, E, A>(server: RealEngineServer, transport: T) -> anyhow::Result<()>
where
    T: rmcp::transport::IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let running = server.serve(transport).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc_4648() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }
}
