//! Model Context Protocol (MCP) server over newline-delimited JSON-RPC 2.0.
//!
//! The adapter exists because a coding agent's shell usually runs one command at a time:
//! `agentforge serve` needs a held-open pipe to preserve session state. An MCP server keeps
//! one [`Session`] alive for the lifetime of the client connection, and exposes the engine's
//! own operations as tools, so state, revisions and episodes persist across calls.
//!
//! The implementation is deliberately small and synchronous: JSON-RPC 2.0 over stdin/stdout
//! with one request in flight, no async runtime and no SDK. Tool metadata comes from the
//! same operation catalog that generates `discover` and the published schemas.
//!
//! # Protocol revision
//!
//! This server implements the legacy (initialize-handshake) MCP revision
//! [`MCP_PROTOCOL_VERSION`]. Modern clients that probe with `server/discover` receive a
//! method-not-found error and, per the specification, fall back to `initialize`. The modern
//! per-request-metadata revision is not implemented.
//!
//! # Tool names
//!
//! MCP clients impose `[A-Za-z0-9_-]` names, so dots in operation names become underscores:
//! `entity.create` is exposed as `entity_create`. The mapping is one-way; `discover` and the
//! protocol references keep the dotted names.
//!
//! # Result shape
//!
//! Every successful call returns `{"revision": <session revision>, "result": <engine result>}`
//! as both pretty JSON text and structured content. Every engine error is an `isError: true`
//! result carrying `{"revision": ..., "error": {code, message, details, hint}}`, so the agent
//! sees the same actionable error body as a JSON Lines client. JSON-RPC errors are reserved
//! for transport-level problems: malformed messages, unknown methods and unknown tools.

use std::io::{self, BufRead, Write};

use serde_json::{Map, Value, json};

use crate::Role;
use crate::lines::{LineRead, read_bounded_line};
use crate::protocol::{PROTOCOL_VERSION, ResponseStatus};
use crate::server::{ServeStats, Session};

/// Legacy MCP revision implemented by this server.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// MCP revisions this build accepts. Only the advertised revision is served; a client that
/// requests another one is answered with [`MCP_PROTOCOL_VERSION`] and decides whether to
/// continue.
pub const SUPPORTED_MCP_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION];

/// Largest accepted MCP message, in bytes (excluding the newline).
///
/// This is larger than the engine's 1 MiB request line so the wrapper itself does not
/// reject anything the engine could have accepted.
pub const MAX_MCP_MESSAGE_BYTES: usize = 4 << 20;

const JSONRPC_VERSION: &str = "2.0";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_NOT_INITIALIZED: i64 = -32002;

/// One MCP tool: a name, the engine operation it calls, and the argument schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// MCP tool name, for example `entity_create`.
    pub name: String,
    /// Engine operation the tool calls, for example `entity.create`.
    pub operation: String,
    /// One-line description, taken from the operation catalog.
    pub description: String,
    /// Standalone JSON Schema of the arguments object.
    pub input_schema: Value,
}

/// Identity and guidance reported during initialization.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// Name reported in `serverInfo.name`.
    pub name: String,
    /// Engine version reported in `serverInfo.version`.
    pub version: String,
    /// Text reported in `instructions`.
    pub instructions: String,
}

/// Tools for a grid session, filtered to the operations `role` may call.
pub fn grid_tools(role: Role) -> Vec<Tool> {
    use crate::protocol::catalog::{self, OpId};

    OpId::ALL
        .into_iter()
        .filter(|op| op.allows(role))
        .map(|op| {
            let spec = op.spec();
            Tool {
                name: tool_name(spec.name),
                operation: spec.name.to_owned(),
                description: spec.summary.to_owned(),
                input_schema: catalog::standalone_schema(spec.arguments_root),
            }
        })
        .collect()
}

/// Tools for a physics session, filtered to the operations `role` may call.
pub fn physics_tools(role: Role) -> Vec<Tool> {
    crate::physics::operations(role)
        .into_iter()
        .map(|operation| Tool {
            name: tool_name(&operation.name),
            operation: operation.name.clone(),
            description: operation.summary,
            input_schema: operation.input_schema,
        })
        .collect()
}

fn tool_name(operation: &str) -> String {
    operation.replace('.', "_")
}

/// Serves MCP messages from `input` until end of input, writing replies to `output`.
///
/// `session` keeps its state for the whole loop, which is the point of the adapter: a
/// revision, open project or running episode survives across tool calls. Returns an error
/// only if the streams themselves fail.
pub fn serve<S: Session, R: BufRead, W: Write>(
    session: &mut S,
    tools: Vec<Tool>,
    info: ServerInfo,
    mut input: R,
    mut output: W,
) -> io::Result<ServeStats> {
    let mut state = State {
        tools,
        info,
        initialized: false,
        stats: ServeStats::default(),
    };
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let reply = match read_bounded_line(&mut input, &mut buffer, MAX_MCP_MESSAGE_BYTES)? {
            LineRead::Eof => return Ok(state.stats),
            LineRead::TooLong => {
                state.stats.requests += 1;
                Some(error_response(
                    Value::Null,
                    INVALID_REQUEST,
                    format!("message exceeds {MAX_MCP_MESSAGE_BYTES} bytes and was skipped"),
                    None,
                ))
            }
            LineRead::Line => {
                if buffer.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                state.stats.requests += 1;
                match std::str::from_utf8(&buffer) {
                    Ok(line) => state.handle_line(session, line),
                    Err(error) => Some(error_response(
                        Value::Null,
                        PARSE_ERROR,
                        format!("message is not valid UTF-8: {error}"),
                        None,
                    )),
                }
            }
        };
        if let Some(reply) = reply {
            if reply.get("error").is_some() {
                state.stats.errors += 1;
            }
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

struct State {
    tools: Vec<Tool>,
    info: ServerInfo,
    initialized: bool,
    stats: ServeStats,
}

impl State {
    fn handle_line<S: Session>(&mut self, session: &mut S, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                return Some(error_response(
                    Value::Null,
                    PARSE_ERROR,
                    format!("invalid JSON: {error}"),
                    None,
                ));
            }
        };
        self.handle_message(session, message)
    }

    fn handle_message<S: Session>(&mut self, session: &mut S, message: Value) -> Option<Value> {
        if message.is_array() {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "JSON-RPC batching is not supported",
                None,
            ));
        }
        let Some(object) = message.as_object() else {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "a message must be a JSON object",
                None,
            ));
        };
        let id = object.get("id").cloned();
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
            return Some(error_response(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
                None,
            ));
        }
        let method = match object.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => {
                return Some(error_response(
                    id.unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    "missing method",
                    None,
                ));
            }
        };
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
        let Some(id) = id else {
            // Notifications never receive a response. The engine has no server-initiated
            // messages, so only the lifecycle notification matters and it needs no state.
            return None;
        };
        if !self.initialized && method != "initialize" && method != "ping" {
            return Some(error_response(
                id,
                SERVER_NOT_INITIALIZED,
                "server not initialized; send initialize first",
                None,
            ));
        }
        let outcome = match method {
            "initialize" => self.initialize(params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(session, params),
            _ => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method: {method}"),
            )),
        };
        Some(match outcome {
            Ok(result) => success_response(id, result),
            Err(error) => error_response(id, error.code, error.message, error.data),
        })
    }

    fn initialize(&mut self, params: Value) -> Result<Value, RpcError> {
        if self.initialized {
            return Err(RpcError::new(
                INVALID_REQUEST,
                "initialize was already called",
            ));
        }
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                RpcError::new(
                    INVALID_PARAMS,
                    "initialize requires a protocolVersion string",
                )
            })?;
        let negotiated = if SUPPORTED_MCP_PROTOCOL_VERSIONS.contains(&requested) {
            requested
        } else {
            MCP_PROTOCOL_VERSION
        };
        self.initialized = true;
        Ok(json!({
            "protocolVersion": negotiated,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": self.info.name, "version": self.info.version},
            "instructions": self.info.instructions,
        }))
    }

    fn tools_list(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                })
            })
            .collect();
        json!({"tools": tools})
    }

    fn tools_call<S: Session>(
        &mut self,
        session: &mut S,
        params: Value,
    ) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call requires a tool name"))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "tool arguments must be a JSON object",
            ));
        }
        let operation = self
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .map(|tool| tool.operation.clone())
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("unknown tool: {name}")))?;
        let request = json!({
            "request_id": "mcp",
            "protocol_version": PROTOCOL_VERSION,
            "operation": operation,
            "arguments": arguments,
        });
        let response = session.handle_line(&request.to_string());
        Ok(match response.status {
            ResponseStatus::Ok => {
                let result = response.result.unwrap_or(Value::Null);
                let structured = json!({"revision": response.revision, "result": result});
                let text =
                    serde_json::to_string_pretty(&structured).unwrap_or_else(|_| "null".to_owned());
                json!({
                    "content": [{"type": "text", "text": text}],
                    "structuredContent": structured,
                    "isError": false,
                })
            }
            ResponseStatus::Error => {
                let body = response.error.map(|error| json!(error)).unwrap_or_else(|| {
                    json!({
                        "code": "internal_error",
                        "message": "the engine reported an error without a body",
                    })
                });
                let structured = json!({"revision": response.revision, "error": body});
                let text =
                    serde_json::to_string_pretty(&structured).unwrap_or_else(|_| "null".to_owned());
                json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": true,
                })
            }
        })
    }
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": JSONRPC_VERSION, "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut error = Map::new();
    error.insert("code".to_owned(), json!(code));
    error.insert("message".to_owned(), json!(message.into()));
    if let Some(data) = data {
        error.insert("data".to_owned(), data);
    }
    json!({"jsonrpc": JSONRPC_VERSION, "id": id, "error": Value::Object(error)})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Host;

    fn initialize(id: u64) -> Value {
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            },
        })
    }

    fn info() -> ServerInfo {
        ServerInfo {
            name: "agentforge-test".to_owned(),
            version: "0".to_owned(),
            instructions: "test instructions".to_owned(),
        }
    }

    fn exchange<S: Session>(session: &mut S, tools: Vec<Tool>, messages: &[Value]) -> Vec<Value> {
        let input = messages
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut output = Vec::new();
        serve(session, tools, info(), input.as_bytes(), &mut output)
            .expect("in-memory streams do not fail");
        String::from_utf8(output)
            .expect("utf-8 output")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each output line is a JSON message"))
            .collect()
    }

    fn grid(messages: &[Value]) -> Vec<Value> {
        let mut host = Host::author();
        let tools = grid_tools(host.role());
        exchange(&mut host, tools, messages)
    }

    fn call(id: u64, name: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })
    }

    #[test]
    fn tool_names_are_unique_snake_case_and_cover_the_role_catalog() {
        let tools = grid_tools(Role::Author);
        assert_eq!(tools.len(), 26);
        let mut names = std::collections::BTreeSet::new();
        for tool in &tools {
            assert!(
                !tool.name.contains('.'),
                "{} is not a valid MCP tool name",
                tool.name
            );
            assert!(
                tool.name.len() <= 64
                    && tool
                        .name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{} is not a valid MCP tool name",
                tool.name
            );
            assert!(names.insert(tool.name.clone()), "duplicate {}", tool.name);
            assert!(!tool.description.is_empty());
            assert_eq!(tool.input_schema["type"], "object");
            assert_eq!(tool.operation.replace('.', "_"), tool.name);
        }
        let player = grid_tools(Role::Player);
        assert_eq!(player.len(), 9);
        assert!(player.iter().all(|tool| !tool.name.starts_with("project_")));
        assert!(player.iter().any(|tool| tool.name == "episode_act"));
    }

    #[test]
    fn initialize_reports_the_legacy_revision_and_tool_capability() {
        let replies = grid(&[initialize(1)]);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(
            replies[0]["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(
            replies[0]["result"]["serverInfo"]["name"],
            "agentforge-test"
        );
        assert!(replies[0]["result"]["capabilities"]["tools"].is_object());
        assert_eq!(replies[0]["result"]["instructions"], "test instructions");
    }

    #[test]
    fn an_unknown_requested_version_is_answered_with_the_supported_one() {
        let mut message = initialize(1);
        message["params"]["protocolVersion"] = json!("1999-01-01");
        let replies = grid(&[message]);
        assert_eq!(
            replies[0]["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
    }

    #[test]
    fn requests_before_initialize_are_rejected_but_ping_is_allowed() {
        let replies = grid(&[
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}),
            initialize(9),
            json!({"jsonrpc": "2.0", "id": 10, "method": "tools/list"}),
        ]);
        assert_eq!(replies.len(), 4);
        assert_eq!(replies[0]["error"]["code"], SERVER_NOT_INITIALIZED);
        assert_eq!(replies[0]["id"], 7);
        assert!(replies[1]["result"].is_object());
        assert_eq!(
            replies[2]["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(
            replies[3]["result"]["tools"]
                .as_array()
                .expect("tools array")
                .len(),
            26
        );
    }

    #[test]
    fn a_second_initialize_is_rejected() {
        let replies = grid(&[initialize(1), initialize(2)]);
        assert_eq!(
            replies[0]["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(replies[1]["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn notifications_receive_no_response() {
        let replies = grid(&[
            initialize(1),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        ]);
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[1]["id"], 2);
    }

    #[test]
    fn tools_call_keeps_session_state_and_returns_structured_content() {
        let replies = grid(&[
            initialize(1),
            call(
                2,
                "project_create",
                json!({"name": "demo", "expected_revision": 0}),
            ),
            call(3, "session_status", json!({})),
        ]);
        assert_eq!(replies[1]["result"]["isError"], false);
        assert_eq!(replies[1]["result"]["structuredContent"]["revision"], 1);
        assert_eq!(
            replies[2]["result"]["structuredContent"]["result"]["revision"],
            1
        );
        assert_eq!(
            replies[2]["result"]["structuredContent"]["result"]["project"]["name"],
            "demo"
        );
        let text = replies[2]["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        let parsed: Value = serde_json::from_str(text).expect("text is JSON");
        assert_eq!(parsed, replies[2]["result"]["structuredContent"]);
    }

    #[test]
    fn engine_errors_are_tool_errors_with_the_full_structured_body() {
        let replies = grid(&[
            initialize(1),
            call(2, "scene_get", json!({"scene": "missing"})),
            call(
                3,
                "project_create",
                json!({"name": "demo", "expected_revision": 0}),
            ),
            call(
                4,
                "project_create",
                json!({"name": "other", "expected_revision": 0}),
            ),
        ]);
        assert_eq!(replies[1]["result"]["isError"], true);
        assert!(
            replies[1]["result"]["structuredContent"].is_null(),
            "errors do not claim structured content"
        );
        let text = replies[1]["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        let parsed: Value = serde_json::from_str(text).expect("error text is JSON");
        assert_eq!(parsed["error"]["code"], "no_project_open");
        assert!(parsed["error"]["hint"].is_string());
        assert_eq!(parsed["revision"], 0);
        assert_eq!(replies[2]["result"]["isError"], false);
        assert_eq!(replies[3]["result"]["isError"], true);
        let text = replies[3]["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        let parsed: Value = serde_json::from_str(text).expect("error text is JSON");
        assert_eq!(parsed["error"]["code"], "stale_revision");
        assert_eq!(parsed["error"]["details"]["current_revision"], 1);
        assert_eq!(parsed["revision"], 1);
    }

    #[test]
    fn unknown_tools_and_bad_arguments_are_protocol_errors() {
        let replies = grid(&[
            initialize(1),
            call(2, "entity_destroy", json!({})),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "session_status", "arguments": []},
            }),
        ]);
        assert_eq!(replies[1]["error"]["code"], INVALID_PARAMS);
        assert!(
            replies[1]["error"]["message"]
                .as_str()
                .expect("message")
                .contains("entity_destroy")
        );
        assert_eq!(replies[2]["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn malformed_messages_get_json_rpc_errors_and_the_session_continues() {
        let mut host = Host::author();
        let tools = grid_tools(host.role());
        let input = concat!(
            "not json\n",
            "[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"resources/list\"}\n",
        );
        let mut output = Vec::new();
        serve(&mut host, tools, info(), input.as_bytes(), &mut output)
            .expect("in-memory streams do not fail");
        let replies: Vec<Value> = String::from_utf8(output)
            .expect("utf-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON"))
            .collect();
        assert_eq!(replies.len(), 4);
        assert_eq!(replies[0]["error"]["code"], PARSE_ERROR);
        assert_eq!(replies[0]["id"], Value::Null);
        assert_eq!(replies[1]["error"]["code"], INVALID_REQUEST);
        assert_eq!(replies[2]["error"]["code"], INVALID_REQUEST);
        assert_eq!(replies[3]["error"]["code"], SERVER_NOT_INITIALIZED);
    }

    #[test]
    fn string_and_numeric_ids_are_echoed_unchanged() {
        let mut initialize = initialize(1);
        initialize["id"] = json!("init");
        let replies = grid(&[
            initialize,
            json!({"jsonrpc": "2.0", "id": "second", "method": "ping"}),
        ]);
        assert_eq!(replies[0]["id"], "init");
        assert_eq!(replies[1]["id"], "second");
    }

    #[test]
    fn physics_tools_expose_the_physics_catalog() {
        use crate::physics::PhysicsHost;

        let tools = physics_tools(Role::Author);
        assert_eq!(tools.len(), 19);
        assert!(tools.iter().any(|tool| tool.name == "scene_set"));
        assert!(tools.iter().any(|tool| tool.name == "episode_act"));
        assert!(tools.iter().all(|tool| !tool.description.is_empty()));
        let player = physics_tools(Role::Player);
        assert_eq!(player.len(), 8);
        assert!(player.iter().all(|tool| tool.name != "scene_set"));
        assert!(player.iter().any(|tool| tool.name == "replay_export"));

        let mut host = PhysicsHost::author(None).expect("author session");
        let replies = exchange(
            &mut host,
            tools,
            &[initialize(1), call(2, "session_status", json!({}))],
        );
        assert_eq!(replies[1]["result"]["isError"], false);
        assert_eq!(
            replies[1]["result"]["structuredContent"]["result"]["role"],
            "author"
        );
        assert_eq!(
            replies[1]["result"]["structuredContent"]["result"]["has_scene"],
            false
        );
    }
}
