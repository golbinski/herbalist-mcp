pub mod tools;

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    ErrorData as McpError, RoleServer, ServerHandler,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tools::ToolContext;

#[derive(Clone)]
pub struct HerbalistServer {
    ctx: Arc<ToolContext>,
}

impl HerbalistServer {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx: Arc::new(ctx) }
    }
}

impl ServerHandler for HerbalistServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "herbalist-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Semantic search and graph navigation over an Obsidian vault. \
                 Use search_notes to discover relevant notes by concept, \
                 get_note to read a note with its graph context, \
                 related_notes / graph_neighbors to traverse the knowledge graph.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _cx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tool_definitions()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _cx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let params: Value = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(Default::default()));

        let result: anyhow::Result<Value> = match request.name.as_ref() {
            "search_notes" => tools::search_notes(&self.ctx, &params),
            "get_note" => tools::get_note(&self.ctx, &params),
            "related_notes" => tools::related_notes(&self.ctx, &params),
            "list_tags" => tools::list_tags(&self.ctx, &params),
            "notes_by_tag" => tools::notes_by_tag(&self.ctx, &params),
            "graph_neighbors" => tools::graph_neighbors(&self.ctx, &params),
            other => Err(anyhow::anyhow!("unknown tool: {}", other)),
        };

        match result {
            Ok(v) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Error: {e}"
            ))])),
        }
    }
}

fn make_tool(name: &'static str, description: &'static str, schema: Value) -> Tool {
    let schema_obj = schema.as_object().cloned().unwrap_or_default();
    Tool::new(name, description, Arc::new(schema_obj))
}

fn tool_definitions() -> Vec<Tool> {
    vec![
        make_tool(
            "search_notes",
            "Semantic + keyword search over the vault. Returns the most relevant note sections.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language search query" },
                    "top_k": { "type": "integer", "description": "Number of results to return (default 10)" }
                },
                "required": ["query"]
            }),
        ),
        make_tool(
            "get_note",
            "Read a note's full content along with its frontmatter, tags, outlinks, and backlinks.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Vault-relative path to the note (e.g. 'Topics/Rust.md')" }
                },
                "required": ["path"]
            }),
        ),
        make_tool(
            "related_notes",
            "Find notes that are structurally similar to a given note based on the wikilink graph.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Vault-relative path to the anchor note" },
                    "top_k": { "type": "integer", "description": "Number of results (default 10)" }
                },
                "required": ["path"]
            }),
        ),
        make_tool(
            "list_tags",
            "List all tags present in the vault (from frontmatter and inline #tags).",
            json!({ "type": "object", "properties": {} }),
        ),
        make_tool(
            "notes_by_tag",
            "Return all note paths that have a given tag.",
            json!({
                "type": "object",
                "properties": {
                    "tag": { "type": "string", "description": "Tag to look up (without leading #)" }
                },
                "required": ["tag"]
            }),
        ),
        make_tool(
            "graph_neighbors",
            "Traverse the wikilink graph from a note up to `depth` hops in both directions.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Vault-relative path to the starting note" },
                    "depth": { "type": "integer", "description": "Number of hops to traverse (default 1, max 5)" }
                },
                "required": ["path"]
            }),
        ),
    ]
}
