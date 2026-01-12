use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Response, Server};

use crate::mcp::bridge::{UiBridge, UiCommand};
use crate::mcp::state::McpState;
use crate::mcp::types::{
    ExportArgs, GainArgs, GainClearArgs, ListFilesArgs, ListToolsResult, LoopArgs, ModeArgs,
    OpenFilesArgs, OpenFolderArgs, PitchArgs, PromptDescriptor, PromptResult, ResourceContent,
    ResourceDescriptor, ScreenshotArgs, SelectionArgs, SpeedArgs, StretchArgs, ToolDescriptor,
    VolumeArgs, WriteLoopArgs,
};

fn mcp_debug_enabled() -> bool {
    std::env::var("NEOWAVES_MCP_DEBUG")
        .ok()
        .map(|v| v != "0")
        .unwrap_or(false)
}

fn log_mcp(debug: bool, msg: &str) {
    if debug {
        eprintln!("[mcp] {msg}");
    }
}

pub struct McpServer {
    pub state: McpState,
    pub bridge: UiBridge,
}

impl McpServer {
    pub fn new(state: McpState, bridge: UiBridge) -> Self {
        Self { state, bridge }
    }

    pub fn list_tools(&self) -> ListToolsResult {
        let schema_empty = json!({ "type": "object", "properties": {}, "additionalProperties": false });
        ListToolsResult {
            tools: vec![
                ToolDescriptor { name: "list_files".into(), description: "List files in the current view".into(), input_schema: json!({"type":"object","properties":{"query":{"type":"string"},"regex":{"type":"boolean"},"limit":{"type":"integer","minimum":1},"offset":{"type":"integer","minimum":0},"include_meta":{"type":"boolean"}},"additionalProperties":false}) },
                ToolDescriptor { name: "get_selection".into(), description: "Get current selection".into(), input_schema: schema_empty.clone() },
                ToolDescriptor { name: "set_selection".into(), description: "Set selection".into(), input_schema: json!({"type":"object","properties":{"paths":{"type":"array","items":{"type":"string"}},"open_tab":{"type":"boolean"}},"additionalProperties":false}) },
                ToolDescriptor { name: "play".into(), description: "Start playback".into(), input_schema: schema_empty.clone() },
                ToolDescriptor { name: "stop".into(), description: "Stop playback".into(), input_schema: schema_empty.clone() },
                ToolDescriptor { name: "set_volume".into(), description: "Set master volume (dB)".into(), input_schema: json!({"type":"object","properties":{"db":{"type":"number"}},"additionalProperties":false}) },
                ToolDescriptor { name: "set_mode".into(), description: "Set playback mode".into(), input_schema: json!({"type":"object","properties":{"mode":{"type":"string"}},"additionalProperties":false}) },
                ToolDescriptor { name: "set_speed".into(), description: "Set speed rate".into(), input_schema: json!({"type":"object","properties":{"rate":{"type":"number"}},"additionalProperties":false}) },
                ToolDescriptor { name: "set_pitch".into(), description: "Set pitch semitones".into(), input_schema: json!({"type":"object","properties":{"semitones":{"type":"number"}},"additionalProperties":false}) },
                ToolDescriptor { name: "set_stretch".into(), description: "Set stretch rate".into(), input_schema: json!({"type":"object","properties":{"rate":{"type":"number"}},"additionalProperties":false}) },
                ToolDescriptor { name: "apply_gain".into(), description: "Apply pending gain".into(), input_schema: json!({"type":"object","properties":{"path":{"type":"string"},"db":{"type":"number"}},"additionalProperties":false}) },
                ToolDescriptor { name: "clear_gain".into(), description: "Clear pending gain".into(), input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"additionalProperties":false}) },
                ToolDescriptor { name: "set_loop_markers".into(), description: "Set loop markers".into(), input_schema: json!({"type":"object","properties":{"path":{"type":"string"},"start_samples":{"type":"integer"},"end_samples":{"type":"integer"}},"additionalProperties":false}) },
                ToolDescriptor { name: "write_loop_markers".into(), description: "Write loop markers".into(), input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"additionalProperties":false}) },
                ToolDescriptor { name: "export_selected".into(), description: "Export selected files".into(), input_schema: json!({"type":"object","properties":{"mode":{"type":"string"},"dest_folder":{"type":"string"},"name_template":{"type":"string"},"conflict":{"type":"string"}},"additionalProperties":false}) },
                ToolDescriptor { name: "open_folder".into(), description: "Open folder".into(), input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"additionalProperties":false}) },
                ToolDescriptor { name: "open_files".into(), description: "Open files".into(), input_schema: json!({"type":"object","properties":{"paths":{"type":"array","items":{"type":"string"}}},"additionalProperties":false}) },
                ToolDescriptor { name: "screenshot".into(), description: "Take screenshot".into(), input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"additionalProperties":false}) },
                ToolDescriptor { name: "get_debug_summary".into(), description: "Get debug summary".into(), input_schema: schema_empty },
            ],
        }
    }

    pub fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        let args = if args.is_null() { json!({}) } else { args };
        let call_ui = |cmd: UiCommand| -> Result<Value> {
            let res = self.bridge.send(cmd)?;
            if res.ok {
                Ok(res.payload)
            } else {
                Err(anyhow!(res.error.unwrap_or_else(|| "tool failed".to_string())))
            }
        };
        match name {
            "list_files" => {
                let args: ListFilesArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::ListFiles(args))
            }
            "get_selection" => call_ui(UiCommand::GetSelection),
            "set_selection" => {
                let args: SelectionArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::SetSelection(args))
            }
            "play" => call_ui(UiCommand::Play),
            "stop" => call_ui(UiCommand::Stop),
            "set_volume" => {
                let args: VolumeArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::SetVolume(args))
            }
            "set_mode" => {
                let args: ModeArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::SetMode(args))
            }
            "set_speed" => {
                let args: SpeedArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::SetSpeed(args))
            }
            "set_pitch" => {
                let args: PitchArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::SetPitch(args))
            }
            "set_stretch" => {
                let args: StretchArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::SetStretch(args))
            }
            "apply_gain" => {
                let args: GainArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::ApplyGain(args))
            }
            "clear_gain" => {
                let args: GainClearArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::ClearGain(args))
            }
            "set_loop_markers" => {
                let args: LoopArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::SetLoopMarkers(args))
            }
            "write_loop_markers" => {
                let args: WriteLoopArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::WriteLoopMarkers(args))
            }
            "export_selected" => {
                let args: ExportArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::Export(args))
            }
            "open_folder" => {
                let args: OpenFolderArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::OpenFolder(args))
            }
            "open_files" => {
                let args: OpenFilesArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::OpenFiles(args))
            }
            "screenshot" => {
                let args: ScreenshotArgs = serde_json::from_value(args)?;
                call_ui(UiCommand::Screenshot(args))
            }
            "get_debug_summary" => call_ui(UiCommand::DebugSummary),
            _ => Err(anyhow!("NOT_FOUND: tool {name}")),
        }
    }

    pub fn list_resources(&self) -> Result<Vec<ResourceDescriptor>> {
        crate::mcp::resources::list_resources(&self.state)
    }

    pub fn read_resource(&self, uri: &str) -> Result<ResourceContent> {
        crate::mcp::resources::read_resource(&self.state, uri)
    }

    pub fn list_prompts(&self) -> Result<Vec<PromptDescriptor>> {
        crate::mcp::prompts::list_prompts()
    }

    pub fn get_prompt(&self, name: &str, args: Value) -> Result<PromptResult> {
        crate::mcp::prompts::get_prompt(name, args)
    }

    fn handle_rpc_request(&self, req: Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let result = (|| -> Result<Value> {
            match method {
                "list_tools" => Ok(serde_json::to_value(self.list_tools()).unwrap_or(json!({}))),
                "call_tool" => {
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("INVALID_ARGS: missing tool name"))?;
                    let args = params
                        .get("arguments")
                        .cloned()
                        .or_else(|| params.get("args").cloned())
                        .unwrap_or(json!({}));
                    self.call_tool(name, args)
                }
                "resources/list" => self
                    .list_resources()
                    .and_then(|res| serde_json::to_value(res).map_err(|e| anyhow!(e))),
                "resources/read" => {
                    let uri = params
                        .get("uri")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("INVALID_ARGS: missing uri"))?;
                    self.read_resource(uri)
                        .and_then(|res| serde_json::to_value(res).map_err(|e| anyhow!(e)))
                }
                "prompts/list" => self
                    .list_prompts()
                    .and_then(|res| serde_json::to_value(res).map_err(|e| anyhow!(e))),
                "prompts/get" => {
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| anyhow!("INVALID_ARGS: missing name"))?;
                    self.get_prompt(&name, params)
                        .and_then(|res| serde_json::to_value(res).map_err(|e| anyhow!(e)))
                }
                _ => Err(anyhow!("NOT_FOUND: method {method}")),
            }
        })();
        match result {
            Ok(payload) => json!({ "jsonrpc": "2.0", "id": id, "result": payload }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": err.to_string() }
            }),
        }
    }

    pub fn run_stdio(self) -> Result<()> {
        use std::io::{BufRead, Write};
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        let debug = mcp_debug_enabled();
        for line in stdin.lock().lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let req: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(err) => {
                    log_mcp(debug, &format!("stdio parse error: {err}"));
                    let _ = writeln!(
                        stdout,
                        "{}",
                        json!({
                            "jsonrpc": "2.0",
                            "id": Value::Null,
                            "error": { "code": -32700, "message": format!("parse error: {err}") }
                        })
                    );
                    stdout.flush()?;
                    continue;
                }
            };
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
            log_mcp(debug, &format!("stdio rpc id={id} method={method}"));
            let resp = self.handle_rpc_request(req);
            let status = if resp.get("error").is_some() { "error" } else { "ok" };
            log_mcp(debug, &format!("stdio rpc id={id} status={status}"));
            let _ = writeln!(stdout, "{}", resp);
            stdout.flush()?;
        }
        Ok(())
    }

    pub fn run_http(self, addr: &str) -> Result<()> {
        let server = Server::http(addr).map_err(|e| anyhow!("http bind failed: {e}"))?;
        let debug = mcp_debug_enabled();
        let cors_headers = [
            Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap(),
            Header::from_bytes("Access-Control-Allow-Headers", "Content-Type").unwrap(),
            Header::from_bytes("Access-Control-Allow-Methods", "POST, OPTIONS").unwrap(),
        ];
        for mut request in server.incoming_requests() {
            let method = request.method().clone();
            let url = request.url().to_string();
            log_mcp(debug, &format!("http {method} {url}"));
            if method == Method::Options {
                let mut response = Response::from_string("");
                for header in &cors_headers {
                    response.add_header(header.clone());
                }
                let _ = request.respond(response);
                continue;
            }
            if method == Method::Get && url == "/health" {
                let mut response = Response::from_string("ok");
                for header in &cors_headers {
                    response.add_header(header.clone());
                }
                let _ = request.respond(response);
                continue;
            }
            if method != Method::Post || url != "/rpc" {
                let mut response = Response::from_string("not found").with_status_code(404);
                for header in &cors_headers {
                    response.add_header(header.clone());
                }
                let _ = request.respond(response);
                continue;
            }
            let mut body = String::new();
            if let Err(err) = request.as_reader().read_to_string(&mut body) {
                log_mcp(debug, &format!("http read error: {err}"));
                let mut response =
                    Response::from_string(format!("read error: {err}")).with_status_code(400);
                for header in &cors_headers {
                    response.add_header(header.clone());
                }
                let _ = request.respond(response);
                continue;
            }
            let req_val: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(err) => {
                    log_mcp(debug, &format!("http parse error: {err}"));
                    let err_val = json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {err}") }
                    });
                    let mut response =
                        Response::from_string(err_val.to_string()).with_status_code(400);
                    for header in &cors_headers {
                        response.add_header(header.clone());
                    }
                    let _ = request.respond(response);
                    continue;
                }
            };
            let id = req_val.get("id").cloned().unwrap_or(Value::Null);
            let rpc_method = req_val.get("method").and_then(|m| m.as_str()).unwrap_or("");
            log_mcp(debug, &format!("http rpc id={id} method={rpc_method}"));
            let resp_val = self.handle_rpc_request(req_val);
            let status = if resp_val.get("error").is_some() { "error" } else { "ok" };
            log_mcp(debug, &format!("http rpc id={id} status={status}"));
            let mut response = Response::from_string(resp_val.to_string());
            for header in &cors_headers {
                response.add_header(header.clone());
            }
            let _ = request.respond(response);
        }
        Ok(())
    }
}
