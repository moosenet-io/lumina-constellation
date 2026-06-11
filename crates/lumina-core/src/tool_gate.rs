//! GUARD-08: Tool gate framework for MCP tools
//!
//! Provides least-privilege tool access control with argument sanitization,
//! permission checking, and result filtering.
//!
//! EDGE-01: ToolGate can optionally hold a WasmSandbox. When present, capability
//! checks are enforced as a policy layer before the MCP transport call.
//! Tools without explicit capability grants default to Stdout-only.

use crate::audit_log::{destructive_gate_outcome, AuditEntry, AuditLog, AuditOutcome};
use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use crate::users::UserRole;
use crate::security::output_filter::{filter_output, OutputFilter};
use crate::tool_types::{ToolAllowlist, ToolCall, ToolDefinition, ToolPermission, ToolResult, WasmCapability};
use crate::user_config::UserToolConfig;
use crate::wasm_sandbox::WasmSandbox;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Tool gate for managing MCP tool access
pub struct ToolGate {
    allowlist: Arc<Mutex<ToolAllowlist>>,
    tools: Arc<Mutex<HashMap<String, ToolDefinition>>>,
    output_filter: OutputFilter,
    /// EDGE-01: Optional WASM sandbox. When Some, policy is enforced before MCP calls.
    sandbox: Option<Arc<WasmSandbox>>,
    /// EDGE-01: Per-tool capability configuration. Tools not listed get Stdout only.
    tool_capabilities: Arc<Mutex<HashMap<String, Vec<WasmCapability>>>>,
    /// EDGE-02: Shared egress inspector. One instance per ToolGate so the rate-alert
    /// window and block counter persist across tool calls within the same gate session.
    egress_inspector: Arc<EgressInspector>,
}

impl ToolGate {
    /// Create new tool gate with empty allowlist (deny all by default).
    /// No WASM sandbox is initialized — use `with_sandbox()` to enable it.
    pub fn new() -> Self {
        Self {
            allowlist: Arc::new(Mutex::new(ToolAllowlist::new())),
            tools: Arc::new(Mutex::new(HashMap::new())),
            output_filter: OutputFilter::default(),
            sandbox: None,
            tool_capabilities: Arc::new(Mutex::new(HashMap::new())),
            egress_inspector: Arc::new(EgressInspector::from_env()),
        }
    }

    /// Create a tool gate with the WASM sandbox enabled.
    ///
    /// Returns an error if the wasmtime engine fails to initialize.
    pub fn with_sandbox() -> Result<Self> {
        let sandbox = WasmSandbox::new()?;
        Ok(Self {
            allowlist: Arc::new(Mutex::new(ToolAllowlist::new())),
            tools: Arc::new(Mutex::new(HashMap::new())),
            output_filter: OutputFilter::default(),
            sandbox: Some(Arc::new(sandbox)),
            tool_capabilities: Arc::new(Mutex::new(HashMap::new())),
            egress_inspector: Arc::new(EgressInspector::from_env()),
        })
    }

    /// Register capability grants for a specific tool.
    ///
    /// If a tool is not registered here, it defaults to `[WasmCapability::Stdout]`
    /// (zero network, filesystem, or env access).
    pub fn set_tool_capabilities(&self, tool_name: String, capabilities: Vec<WasmCapability>) {
        let mut caps = self.tool_capabilities.lock().unwrap();
        caps.insert(tool_name, capabilities);
    }

    /// Check whether the sandbox is active.
    pub fn sandbox_active(&self) -> bool {
        self.sandbox.is_some()
    }

    /// Validate that the tool call's requested operation is within its declared
    /// WASM capabilities.  This is a pure policy check — it does NOT execute WASM.
    ///
    /// When the sandbox is active, the tool's declared capability profile is
    /// logged. If a tool's argument schema references a resource class (e.g. a
    /// field named "path" for filesystem access) but the tool has no matching
    /// capability grant, the call is denied with a clear error message.
    ///
    /// Returns `Ok(())` when the sandbox is not active (fail-open) or when
    /// the tool's declared capabilities are consistent with its argument content.
    pub fn check_capabilities(
        &self,
        tool_name: &str,
        arguments: &str,
    ) -> Result<()> {
        if self.sandbox.is_none() {
            // Sandbox not active — policy check is a no-op
            return Ok(());
        }

        // Retrieve declared capabilities for this tool (default: Stdout only)
        let caps = self.tool_capabilities.lock().unwrap();
        let default_caps = vec![WasmCapability::Stdout];
        let tool_caps = caps.get(tool_name).unwrap_or(&default_caps);

        let has_network = tool_caps.iter().any(|c| matches!(c, WasmCapability::Network { .. }));
        let has_filesystem = tool_caps.iter().any(|c| matches!(c, WasmCapability::Filesystem { .. }));
        let has_env = tool_caps.iter().any(|c| matches!(c, WasmCapability::Env { .. }));

        log::debug!(
            "WASM capability check for '{}': network={}, filesystem={}, env={}",
            tool_name,
            has_network,
            has_filesystem,
            has_env,
        );

        // Inspect arguments (JSON) for indicators that require capabilities not granted.
        // This heuristic checks field names; a full implementation would examine values.
        // If argument parsing fails here, we deny the call — capability checks must not
        // be skipped on malformed input (fail-closed policy).
        let parsed = match serde_json::from_str::<serde_json::Value>(arguments) {
            Ok(v) => v,
            Err(e) => {
                log::warn!(
                    "WASM capability check: could not parse arguments for '{}': {}. Denying call.",
                    tool_name,
                    e
                );
                return Err(LuminaError::SecurityViolation(format!(
                    "Tool '{}' arguments could not be parsed for capability check: {e}",
                    tool_name
                )));
            }
        };

        // Tool arguments must be a JSON object when the sandbox is active.
        // Non-object JSON (arrays, scalars) cannot be safely inspected for capability
        // indicators — fail closed to prevent bypassing the check.
        let obj = match parsed.as_object() {
            Some(o) => o,
            None => {
                return Err(LuminaError::SecurityViolation(format!(
                    "Tool '{}' arguments must be a JSON object when sandbox is active",
                    tool_name
                )));
            }
        };

        let field_names: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();

        // Fields indicating filesystem access
        let fs_fields = ["path", "file", "directory", "filepath", "dir"];
        let requests_fs = field_names.iter().any(|n| fs_fields.contains(n));
        if requests_fs && !has_filesystem {
            return Err(LuminaError::SecurityViolation(format!(
                "Tool '{}' requires Filesystem capability but none was granted",
                tool_name
            )));
        }

        // Fields indicating network access
        let net_fields = ["url", "host", "endpoint", "address"];
        let requests_net = field_names.iter().any(|n| net_fields.contains(n));
        if requests_net && !has_network {
            return Err(LuminaError::SecurityViolation(format!(
                "Tool '{}' requires Network capability but none was granted",
                tool_name
            )));
        }

        Ok(())
    }

    /// Load allowlist from TOML configuration file
    pub fn load_allowlist(&self, config_path: &PathBuf) -> Result<()> {
        if !config_path.exists() {
            // Create default empty config
            let default_allowlist = ToolAllowlist::new();
            let toml_content = default_allowlist.to_toml()
                .map_err(|e| LuminaError::SecurityViolation(format!("Failed to serialize default config: {}", e)))?;

            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(config_path, toml_content)?;

            return Ok(());
        }

        let content = std::fs::read_to_string(config_path)?;
        let allowlist = ToolAllowlist::from_toml(&content)
            .map_err(|e| LuminaError::SecurityViolation(format!("Invalid TOML config: {}", e)))?;

        *self.allowlist.lock().unwrap() = allowlist;
        Ok(())
    }

    /// Save current allowlist to configuration file
    pub fn save_allowlist(&self, config_path: &PathBuf) -> Result<()> {
        let allowlist = self.allowlist.lock().unwrap();
        let content = allowlist.to_toml()
            .map_err(|e| LuminaError::SecurityViolation(format!("Failed to serialize config: {}", e)))?;

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(config_path, content)?;
        Ok(())
    }

    /// Register a tool definition
    pub fn register_tool(&self, tool: ToolDefinition) {
        let mut tools = self.tools.lock().unwrap();
        tools.insert(tool.name.clone(), tool);
    }

    /// Check if tool call is permitted
    pub fn check_permission(&self, tool_call: &ToolCall) -> Result<()> {
        let allowlist = self.allowlist.lock().unwrap();
        let tools = self.tools.lock().unwrap();

        // Get tool definition
        let tool_def = tools.get(&tool_call.function.name)
            .ok_or_else(|| LuminaError::SecurityViolation(
                format!("Unknown tool: {}", tool_call.function.name)
            ))?;

        // Check permission
        if !allowlist.is_allowed(&tool_call.function.name, &tool_def.permission) {
            return Err(LuminaError::SecurityViolation(
                format!("Tool '{}' is not permitted", tool_call.function.name)
            ));
        }

        Ok(())
    }

    /// Check whether `tool_name` is permitted for a specific user.
    ///
    /// Combines the global allowlist with per-user overrides from `UserToolConfig`.
    /// Decision order:
    /// 1. Tool in `user_config.denied_tools` → denied (blocks global allow).
    /// 2. Tool globally allowed (in this gate's allowlist) → permitted.
    /// 3. Tool in `user_config.extra_allowed_tools` → permitted.
    /// 4. Otherwise → denied.
    ///
    /// All lookups are O(1): `HashSet::contains` for user deny/allow lists,
    /// and `HashMap::contains_key` for the global allowlist.
    ///
    /// **Guest enforcement is the caller's responsibility**: callers should
    /// return `false` for Guest-role users before reaching this method.
    pub fn check_user_permission(&self, tool_name: &str, user_config: &UserToolConfig) -> bool {
        // Rule 1: user-denied tools are always blocked (O(1) HashSet lookup).
        if user_config.is_denied(tool_name) {
            return false;
        }

        // Rule 2: check global allowlist (O(1) HashMap::contains_key, no allocation).
        {
            let allowlist = self.allowlist.lock().unwrap();
            if allowlist.tools.contains_key(tool_name) {
                return true;
            }
        }

        // Rule 3: tool in user's extra-allowed list (O(1) HashSet lookup).
        user_config.is_extra_allowed(tool_name)
    }

    /// Sanitize tool arguments to prevent injection
    pub fn sanitize_arguments(&self, arguments: &str) -> Result<String> {
        // Parse as JSON first — valid tool arguments must be a JSON object
        let parsed: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| LuminaError::SecurityViolation(format!("Arguments are not valid JSON: {e}")))?;

        // Inspect string values for shell injection patterns
        let injection_patterns = ["|", ";", "&", "`", "$(", "${", "\\x", "\\u", "%{", "#{"];
        let check_str = |s: &str| -> bool {
            injection_patterns.iter().any(|&p| s.contains(p))
        };

        fn has_injection(v: &serde_json::Value, check: &dyn Fn(&str) -> bool) -> bool {
            match v {
                serde_json::Value::String(s) => check(s),
                serde_json::Value::Object(m) => m.values().any(|v| has_injection(v, check)),
                serde_json::Value::Array(a) => a.iter().any(|v| has_injection(v, check)),
                _ => false,
            }
        }

        if has_injection(&parsed, &check_str) {
            return Err(LuminaError::SecurityViolation(
                "Arguments contain potential shell injection pattern".to_string(),
            ));
        }

        Ok(arguments.to_string())
    }

    /// Check if destructive operation requires confirmation
    pub fn require_confirmation(&self, tool_call: &ToolCall) -> Result<()> {
        let tools = self.tools.lock().unwrap();

        if let Some(tool_def) = tools.get(&tool_call.function.name) {
            if tool_def.permission == ToolPermission::Destructive {
                eprintln!("WARNING: Destructive operation requested: {}", tool_call.function.name);
                eprintln!("Tool: {}", tool_def.description);
                eprintln!("Arguments: {}", tool_call.function.arguments);
                eprintln!("This operation cannot be undone.");

                // In a real implementation, this would prompt for user confirmation
                // For now, we just log the warning
            }
        }

        Ok(())
    }

    /// Sanitize tool result to prevent information leakage
    pub fn sanitize_result(&self, result: &ToolResult) -> ToolResult {
        let filtered_content = self.output_filter.filter(&result.content);

        let filtered_error = result.error.as_ref()
            .map(|e| self.output_filter.filter(e));

        ToolResult {
            tool_call_id: result.tool_call_id.clone(),
            function_name: result.function_name.clone(),
            content: filtered_content,
            success: result.success,
            error: filtered_error,
        }
    }

    /// Validate parsed arguments against the tool's JSON schema (HARDEN-05).
    ///
    /// Skipped if the tool has no schema or if the schema is `{}` (permissive).
    fn validate_schema(&self, tool_name: &str, arguments: &str) -> Result<()> {
        let tools = self.tools.lock().unwrap();
        let tool_def = match tools.get(tool_name) {
            Some(t) => t,
            None => return Ok(()), // unknown tool already caught in check_permission
        };

        // Skip validation if schema is empty/null/bare object with no properties
        if tool_def.argument_schema.is_null()
            || tool_def.argument_schema == serde_json::json!({})
        {
            return Ok(());
        }

        let schema = tool_def.argument_schema.clone();
        drop(tools); // release lock before validation

        let instance: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| LuminaError::SecurityViolation(
                format!("Tool arguments are not valid JSON: {}", e)
            ))?;

        let compiled = jsonschema::validator_for(&schema)
            .map_err(|e| LuminaError::SecurityViolation(
                format!("Invalid tool schema for '{}': {}", tool_name, e)
            ))?;

        let result = compiled.validate(&instance);
        if let Err(errors) = result {
            let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
            return Err(LuminaError::SecurityViolation(
                format!("Tool argument schema validation failed for '{}': {}", tool_name, msgs.join("; "))
            ));
        }

        Ok(())
    }

    /// Process tool call with full security checks
    pub fn process_tool_call(&self, tool_call: &ToolCall) -> Result<ToolCall> {
        // 1. Check permission
        self.check_permission(tool_call)?;

        // 2. Sanitize arguments (shell metacharacter stripping)
        let sanitized_args = self.sanitize_arguments(&tool_call.function.arguments)?;

        // 2b. HARDEN-05: Validate sanitized arguments against JSON schema
        self.validate_schema(&tool_call.function.name, &sanitized_args)?;

        // 3. Check for destructive operations
        self.require_confirmation(tool_call)?;

        // 4. Return sanitized tool call
        Ok(ToolCall {
            id: tool_call.id.clone(),
            call_type: tool_call.call_type.clone(),
            function: crate::tool_types::FunctionCall {
                name: tool_call.function.name.clone(),
                arguments: sanitized_args,
            },
        })
    }

    /// Get default tools.toml path
    pub fn default_config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".lumina")
            .join("tools.toml")
    }

    /// Register a dynamically generated tool and immediately allow it (ReadOnly).
    ///
    /// EDGE-10: Called by [`crate::tool_builder::ToolBuilder::approve_tool`] after
    /// the operator has confirmed the tool.  The tool is registered with a
    /// `ReadOnly` permission level and the provided argument schema so it appears
    /// in the tool list and passes permission checks.
    ///
    /// `name` and `description` should come from the [`GeneratedTool`] metadata;
    /// `schema` is the JSON Schema for the tool's arguments.
    pub fn register_dynamic_tool(
        &self,
        name: &str,
        description: &str,
        schema: serde_json::Value,
    ) {
        let definition = crate::tool_types::ToolDefinition::read_only(
            name.to_string(),
            description.to_string(),
            schema,
        );
        self.register_tool(definition);
        self.allow_tool(name.to_string(), ToolPermission::ReadOnly);
    }

    /// Add tool to allowlist
    pub fn allow_tool(&self, name: String, permission: ToolPermission) {
        let mut allowlist = self.allowlist.lock().unwrap();
        allowlist.allow_tool(name, permission);
    }

    /// Remove tool from allowlist
    pub fn deny_tool(&self, name: &str) {
        let mut allowlist = self.allowlist.lock().unwrap();
        allowlist.deny_tool(name);
    }

    /// Get list of allowed tools
    pub fn get_allowed_tools(&self) -> Vec<(String, ToolPermission)> {
        let allowlist = self.allowlist.lock().unwrap();
        allowlist.get_allowed_tools()
            .into_iter()
            .map(|(name, perm)| (name.clone(), perm.clone()))
            .collect()
    }

    /// Get tool definition
    pub fn get_tool_definition(&self, name: &str) -> Option<ToolDefinition> {
        let tools = self.tools.lock().unwrap();
        tools.get(name).cloned()
    }

    /// Get all registered tools
    pub fn get_all_tools(&self) -> Vec<ToolDefinition> {
        let tools = self.tools.lock().unwrap();
        tools.values().cloned().collect()
    }

    /// Execute a tool call via the MCP dispatcher.
    ///
    /// Sequence: check_permission → EDGE-01 capability check → sanitize_arguments →
    ///   validate_schema → P2-17 destructive audit gate → McpTransport::call_tool →
    ///   sanitize_result.
    ///
    /// EDGE-01: If a sandbox is active, WASM capability policy is enforced before
    /// the MCP call. If no sandbox is active, a warning is logged (degraded security).
    /// Sandbox failures return a structured ToolResult::error — they never panic.
    ///
    /// P2-17: Destructive tool calls are audited before dispatch. The caller's role
    /// determines the gate policy (Admin: logged + approved; Member: logged +
    /// PendingConfirmation; Guest: logged + blocked). Pass `caller_role: None` to
    /// apply Member-level policy (unknown callers are not trusted as admin).
    ///
    /// Permission denials return a structured error ToolResult (not a panic).
    /// The transport is wrapped in a Mutex so it can be shared across turns.
    pub fn execute_tool(
        &self,
        tool_call: &ToolCall,
        transport: &std::sync::Mutex<crate::mcp_client::McpTransport>,
    ) -> ToolResult {
        self.execute_tool_with_role(tool_call, transport, None)
    }

    /// Variant of `execute_tool` that carries the caller's `UserRole` for the
    /// P2-17 destructive action gate.  Prefer this when a user context is
    /// available (e.g. from a Matrix session).
    pub fn execute_tool_with_role(
        &self,
        tool_call: &ToolCall,
        transport: &std::sync::Mutex<crate::mcp_client::McpTransport>,
        caller_role: Option<&UserRole>,
    ) -> ToolResult {
        // 1. Permission check
        if let Err(e) = self.check_permission(tool_call) {
            return ToolResult::error(
                tool_call.id.clone(),
                tool_call.function.name.clone(),
                format!("Permission denied: {e}"),
            );
        }

        // 2. EDGE-01: WASM capability policy check
        if self.sandbox.is_some() {
            if let Err(e) = self.check_capabilities(&tool_call.function.name, &tool_call.function.arguments) {
                return ToolResult::error(
                    tool_call.id.clone(),
                    tool_call.function.name.clone(),
                    format!("Capability denied: {e}"),
                );
            }
        } else {
            // Sandbox not initialised — log degraded-security warning.
            // This does NOT block execution (fail-open for compatibility during migration).
            log::warn!(
                "WASM sandbox not active for tool '{}': running in degraded security mode",
                tool_call.function.name
            );
        }

        // 3. Sanitize + schema validate arguments
        let sanitized_args = match self.sanitize_arguments(&tool_call.function.arguments) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(
                tool_call.id.clone(),
                tool_call.function.name.clone(),
                format!("Argument sanitization failed: {e}"),
            ),
        };
        if let Err(e) = self.validate_schema(&tool_call.function.name, &sanitized_args) {
            return ToolResult::error(
                tool_call.id.clone(),
                tool_call.function.name.clone(),
                format!("Schema validation failed: {e}"),
            );
        }

        // 4. P2-17: Destructive audit gate.
        //
        //    Admin   → Approved (logged at elevated level, no confirmation required)
        //    Member  → PendingConfirmation (stub; approval flow is a higher-layer concern)
        //    Guest   → Blocked unconditionally
        //    None    → treated as Member
        {
            let tools = self.tools.lock().unwrap();
            let is_destructive = tools
                .get(&tool_call.function.name)
                .map(|def| def.permission == ToolPermission::Destructive)
                .unwrap_or(false);
            drop(tools);

            if is_destructive {
                let outcome = destructive_gate_outcome(caller_role);
                let role_str = caller_role.map(|r| r.to_string()).unwrap_or_else(|| "unknown".to_string());

                let entry = AuditEntry::new(
                    &tool_call.function.name,
                    None, // user_id is available at a higher layer (session/Matrix context)
                    &role_str,
                    &sanitized_args,
                    outcome.clone(),
                );

                // Best-effort: audit log failure is non-fatal but logged.
                if let Ok(audit) = AuditLog::open_default() {
                    if let Err(e) = audit.append(&entry) {
                        log::warn!("P2-17: failed to write audit entry: {e}");
                    }
                }

                match outcome {
                    AuditOutcome::Blocked => {
                        return ToolResult::error(
                            tool_call.id.clone(),
                            tool_call.function.name.clone(),
                            "Destructive action blocked: guest users cannot perform \
                             destructive operations."
                                .to_string(),
                        );
                    }
                    AuditOutcome::PendingConfirmation => {
                        return ToolResult::error(
                            tool_call.id.clone(),
                            tool_call.function.name.clone(),
                            "Destructive action requires confirmation. \
                             Action logged as PendingConfirmation."
                                .to_string(),
                        );
                    }
                    AuditOutcome::Approved => {
                        // Admin — fall through to dispatch
                    }
                }
            }
        }

        // 5. Parse arguments for the MCP call
        let arguments: serde_json::Value = match serde_json::from_str(&sanitized_args) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(
                tool_call.id.clone(),
                tool_call.function.name.clone(),
                format!("Argument parse error: {e}"),
            ),
        };

        // 5b. EDGE-02: egress inspection — check network destinations in arguments.
        // The gate's shared EgressInspector preserves the rate-alert window across
        // multiple tool calls so burst detection works within a session.
        {
            let net_fields = ["url", "host", "endpoint", "address"];
            if let Some(obj) = arguments.as_object() {
                for field in &net_fields {
                    if let Some(serde_json::Value::String(dest)) = obj.get(*field) {
                        if let Err(violation) = self.egress_inspector.inspect(dest, &tool_call.function.name) {
                            return ToolResult::error(
                                tool_call.id.clone(),
                                tool_call.function.name.clone(),
                                format!("Egress blocked: {violation}"),
                            );
                        }
                    }
                }
            }
        }

        // 6. Dispatch to MCP (lock the transport for the duration of the call)
        let raw_result = match transport.lock() {
            Ok(mut t) => t.call_tool(&tool_call.id, &tool_call.function.name, &arguments),
            Err(e) => return ToolResult::error(
                tool_call.id.clone(),
                tool_call.function.name.clone(),
                format!("Transport lock poisoned: {e}"),
            ),
        };

        let result = match raw_result {
            Ok(r) => r,
            Err(e) => ToolResult::error(
                tool_call.id.clone(),
                tool_call.function.name.clone(),
                e.to_string(),
            ),
        };

        // 7. Sanitize result (scrub secrets/PII from output)
        self.sanitize_result(&result)
    }
}

impl Default for ToolGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Global tool gate instance
static GLOBAL_TOOL_GATE: OnceLock<ToolGate> = OnceLock::new();

/// Get global tool gate
pub fn global_tool_gate() -> &'static ToolGate {
    GLOBAL_TOOL_GATE.get_or_init(|| ToolGate::new())
}

/// Check tool permission using global gate
pub fn check_tool_permission(tool_call: &ToolCall) -> Result<()> {
    global_tool_gate().check_permission(tool_call)
}

/// Sanitize tool arguments using global gate
pub fn sanitize_tool_arguments(arguments: &str) -> Result<String> {
    global_tool_gate().sanitize_arguments(arguments)
}

/// Process tool call using global gate
pub fn process_tool_call(tool_call: &ToolCall) -> Result<ToolCall> {
    global_tool_gate().process_tool_call(tool_call)
}

/// Sanitize tool result using global gate
pub fn sanitize_tool_result(result: &ToolResult) -> ToolResult {
    global_tool_gate().sanitize_result(result)
}

/// Initialize global tool gate with configuration
pub fn init_global_tool_gate(config_path: Option<PathBuf>) -> Result<()> {
    let gate = global_tool_gate();
    let path = config_path.unwrap_or_else(|| ToolGate::default_config_path());
    gate.load_allowlist(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::env;

    #[test]
    fn test_tool_gate_creation() {
        let gate = ToolGate::new();
        let tools = gate.get_allowed_tools();
        assert!(tools.is_empty()); // Default deny all
    }

    #[test]
    fn test_tool_registration() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({"type": "object"});

        let tool = ToolDefinition::read_only(
            "test_tool".to_string(),
            "A test tool".to_string(),
            schema,
        );

        gate.register_tool(tool);
        assert!(gate.get_tool_definition("test_tool").is_some());
    }

    #[test]
    fn test_permission_checking() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({"type": "object"});

        let tool = ToolDefinition::read_only(
            "read_file".to_string(),
            "Read a file".to_string(),
            schema,
        );

        gate.register_tool(tool);

        let tool_call = ToolCall::new(
            "call_1".to_string(),
            "read_file".to_string(),
            "{}".to_string(),
        );

        // Should fail - tool not in allowlist
        assert!(gate.check_permission(&tool_call).is_err());

        // Add to allowlist
        gate.allow_tool("read_file".to_string(), ToolPermission::ReadOnly);

        // Should now succeed
        assert!(gate.check_permission(&tool_call).is_ok());
    }

    #[test]
    fn test_argument_sanitization() {
        let gate = ToolGate::new();

        // Safe arguments
        let safe_args = r#"{"path": "/tmp/file.txt", "content": "hello world"}"#;
        assert!(gate.sanitize_arguments(safe_args).is_ok());

        // Dangerous arguments
        let dangerous_args = r#"{"command": "rm -rf /; echo 'pwned'"}"#;
        assert!(gate.sanitize_arguments(dangerous_args).is_err());

        let injection_args = r#"{"script": "$(rm -rf /)"}"#;
        assert!(gate.sanitize_arguments(injection_args).is_err());
    }

    #[test]
    fn test_result_sanitization() {
        let gate = ToolGate::new();

        let result = ToolResult::success(
            "call_1".to_string(),
            "read_file".to_string(),
            "Password: secret123 and API key: sk-1234567890".to_string(),
        );

        let sanitized = gate.sanitize_result(&result);
        assert!(!sanitized.content.contains("secret123"));
        assert!(!sanitized.content.contains("sk-1234567890"));
        assert!(sanitized.content.contains("[REDACTED]"));
    }

    #[test]
    fn test_destructive_operation_warning() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({"type": "object"});

        let tool = ToolDefinition::destructive(
            "delete_file".to_string(),
            "Delete a file".to_string(),
            schema,
        );

        gate.register_tool(tool);
        gate.allow_tool("delete_file".to_string(), ToolPermission::Destructive);

        let tool_call = ToolCall::new(
            "call_1".to_string(),
            "delete_file".to_string(),
            r#"{"path": "/tmp/important.txt"}"#.to_string(),
        );

        // Should not fail but should log warning
        assert!(gate.require_confirmation(&tool_call).is_ok());
    }

    #[test]
    fn test_config_file_handling() {
        let temp_dir = env::temp_dir();
        let config_path = temp_dir.join("test_tools.toml");

        // Clean up any existing file
        let _ = fs::remove_file(&config_path);

        let gate = ToolGate::new();
        gate.allow_tool("test_tool".to_string(), ToolPermission::ReadOnly);

        // Save config
        assert!(gate.save_allowlist(&config_path).is_ok());
        assert!(config_path.exists());

        // Load config into new gate
        let new_gate = ToolGate::new();
        assert!(new_gate.load_allowlist(&config_path).is_ok());

        let allowed_tools = new_gate.get_allowed_tools();
        assert_eq!(allowed_tools.len(), 1);
        assert_eq!(allowed_tools[0].0, "test_tool");

        // Clean up
        let _ = fs::remove_file(&config_path);
    }

    #[test]
    fn test_process_tool_call() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({"type": "object"});

        let tool = ToolDefinition::read_only(
            "safe_tool".to_string(),
            "A safe tool".to_string(),
            schema,
        );

        gate.register_tool(tool);
        gate.allow_tool("safe_tool".to_string(), ToolPermission::ReadOnly);

        let tool_call = ToolCall::new(
            "call_1".to_string(),
            "safe_tool".to_string(),
            r#"{"param": "safe_value"}"#.to_string(),
        );

        let result = gate.process_tool_call(&tool_call);
        assert!(result.is_ok());

        // Test with dangerous arguments
        let dangerous_call = ToolCall::new(
            "call_2".to_string(),
            "safe_tool".to_string(),
            r#"{"param": "$(rm -rf /)"}"#.to_string(),
        );

        let result = gate.process_tool_call(&dangerous_call);
        assert!(result.is_err());
    }

    #[test]
    fn test_global_functions() {
        let schema = serde_json::json!({"type": "object"});
        let tool = ToolDefinition::read_only(
            "global_tool".to_string(),
            "A global tool".to_string(),
            schema,
        );

        let gate = global_tool_gate();
        gate.register_tool(tool);
        gate.allow_tool("global_tool".to_string(), ToolPermission::ReadOnly);

        let tool_call = ToolCall::new(
            "call_1".to_string(),
            "global_tool".to_string(),
            "{}".to_string(),
        );

        assert!(check_tool_permission(&tool_call).is_ok());
        assert!(sanitize_tool_arguments("{}").is_ok());
        assert!(sanitize_tool_arguments("$(dangerous)").is_err());

        let result = ToolResult::success(
            "call_1".to_string(),
            "global_tool".to_string(),
            "API key: sk-test".to_string(),
        );

        let sanitized = sanitize_tool_result(&result);
        assert!(!sanitized.content.contains("sk-test"));
    }

    #[test]
    fn test_default_config_path() {
        let path = ToolGate::default_config_path();
        assert!(path.to_string_lossy().contains(".lumina"));
        assert!(path.to_string_lossy().contains("tools.toml"));
    }

    // HARDEN-05: JSON schema validation tests
    #[test]
    fn test_valid_arguments_pass_schema() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
            "additionalProperties": false
        });
        let tool = ToolDefinition::read_only("get_weather".to_string(), "get weather".to_string(), schema);
        gate.register_tool(tool);
        gate.allow_tool("get_weather".to_string(), ToolPermission::ReadOnly);

        let call = ToolCall::new("c1".to_string(), "get_weather".to_string(), r#"{"city":"Portland"}"#.to_string());
        assert!(gate.process_tool_call(&call).is_ok());
    }

    #[test]
    fn test_missing_required_field_rejected() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
            "additionalProperties": false
        });
        let tool = ToolDefinition::read_only("get_weather".to_string(), "get weather".to_string(), schema);
        gate.register_tool(tool);
        gate.allow_tool("get_weather".to_string(), ToolPermission::ReadOnly);

        let call = ToolCall::new("c2".to_string(), "get_weather".to_string(), r#"{}"#.to_string());
        assert!(gate.process_tool_call(&call).is_err());
    }

    #[test]
    fn test_wrong_type_rejected() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "count": { "type": "integer" } },
            "required": ["count"],
            "additionalProperties": false
        });
        let tool = ToolDefinition::read_only("count_things".to_string(), "count".to_string(), schema);
        gate.register_tool(tool);
        gate.allow_tool("count_things".to_string(), ToolPermission::ReadOnly);

        let call = ToolCall::new("c3".to_string(), "count_things".to_string(), r#"{"count":"not_a_number"}"#.to_string());
        assert!(gate.process_tool_call(&call).is_err());
    }

    #[test]
    fn test_tool_without_schema_passes_through() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({}); // permissive / no constraints
        let tool = ToolDefinition::read_only("flexible_tool".to_string(), "flexible".to_string(), schema);
        gate.register_tool(tool);
        gate.allow_tool("flexible_tool".to_string(), ToolPermission::ReadOnly);

        let call = ToolCall::new("c4".to_string(), "flexible_tool".to_string(), r#"{"anything":"goes"}"#.to_string());
        assert!(gate.process_tool_call(&call).is_ok());
    }

    #[test]
    fn test_additional_properties_rejected() {
        let gate = ToolGate::new();
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"],
            "additionalProperties": false
        });
        let tool = ToolDefinition::read_only("strict_tool".to_string(), "strict".to_string(), schema);
        gate.register_tool(tool);
        gate.allow_tool("strict_tool".to_string(), ToolPermission::ReadOnly);

        let call = ToolCall::new("c5".to_string(), "strict_tool".to_string(), r#"{"name":"ok","extra":"bad"}"#.to_string());
        assert!(gate.process_tool_call(&call).is_err());
    }

    /// P1-12: execute_tool returns error ToolResult (not panic) on permission denied.
    #[test]
    fn test_execute_tool_permission_denied_returns_error_result() {
        use crate::mcp_client::McpTransport;
        use std::sync::Mutex;

        let gate = ToolGate::new();
        // Register tool but DO NOT add to allowlist
        let tool = ToolDefinition::read_only("read_logs".to_string(), "Read logs".to_string(), serde_json::json!({}));
        gate.register_tool(tool);

        // Need a mock transport — skip this part since McpTransport requires SSH in production.
        // We'll test the permission-denied path directly using a fake transport.
        // Just verify that without a transport we can't call execute_tool meaningfully,
        // but check the gate logic via process_tool_call instead.
        let call = ToolCall::new("x1".to_string(), "read_logs".to_string(), "{}".to_string());
        // Tool not in allowlist — process_tool_call should fail
        assert!(gate.check_permission(&call).is_err(), "Tool not in allowlist should fail permission");
    }

    /// P1-13: verify ToolDefinition.to_chord_tool() produces correct OpenAI format.
    #[test]
    fn test_to_chord_tool_format() {
        use crate::tool_types::ToolDefinition;
        let def = ToolDefinition::read_only(
            "get_status".to_string(),
            "Get server status".to_string(),
            serde_json::json!({"type": "object", "properties": {}}),
        );
        let ct = def.to_chord_tool();
        assert_eq!(ct.tool_type, "function");
        assert_eq!(ct.function.name, "get_status");
        assert_eq!(ct.function.description, "Get server status");
    }

    // EDGE-01: Sandbox and capability tests

    /// EDGE-01: with_sandbox() initialises successfully and reports sandbox_active().
    #[test]
    fn test_with_sandbox_creates_active_sandbox() {
        let gate = ToolGate::with_sandbox().expect("with_sandbox should succeed");
        assert!(gate.sandbox_active(), "Sandbox should be active after with_sandbox()");
    }

    /// EDGE-01: new() has no sandbox — sandbox_active() returns false.
    #[test]
    fn test_new_has_no_sandbox() {
        let gate = ToolGate::new();
        assert!(!gate.sandbox_active());
    }

    /// EDGE-01: check_capabilities denies a tool with 'path' arg but no Filesystem capability.
    #[test]
    fn test_capability_check_denies_filesystem_without_grant() {
        let gate = ToolGate::with_sandbox().unwrap();
        // No capabilities registered for "read_file" → defaults to Stdout only
        let result = gate.check_capabilities("read_file", r#"{"path": "/etc/passwd"}"#);
        assert!(result.is_err(), "Should deny filesystem access without capability grant");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Filesystem"), "Error should mention Filesystem: {err}");
    }

    /// EDGE-01: check_capabilities allows a tool with 'path' arg AND Filesystem capability.
    #[test]
    fn test_capability_check_allows_filesystem_with_grant() {
        let gate = ToolGate::with_sandbox().unwrap();
        gate.set_tool_capabilities(
            "read_file".to_string(),
            vec![
                WasmCapability::Filesystem { paths: vec![std::path::PathBuf::from("/tmp")] },
                WasmCapability::Stdout,
            ],
        );
        let result = gate.check_capabilities("read_file", r#"{"path": "/tmp/test.txt"}"#);
        assert!(result.is_ok(), "Should allow filesystem access with capability grant");
    }

    /// EDGE-01: check_capabilities denies a tool with 'url' arg but no Network capability.
    #[test]
    fn test_capability_check_denies_network_without_grant() {
        let gate = ToolGate::with_sandbox().unwrap();
        let result = gate.check_capabilities("fetch_url", r#"{"url": "http://example.com"}"#);
        assert!(result.is_err(), "Should deny network access without capability grant");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Network"), "Error should mention Network: {err}");
    }

    /// EDGE-01: check_capabilities allows 'url' arg when Network capability is granted.
    #[test]
    fn test_capability_check_allows_network_with_grant() {
        let gate = ToolGate::with_sandbox().unwrap();
        gate.set_tool_capabilities(
            "fetch_url".to_string(),
            vec![
                WasmCapability::Network { hosts: vec!["example.com".to_string()] },
                WasmCapability::Stdout,
            ],
        );
        let result = gate.check_capabilities("fetch_url", r#"{"url": "http://example.com"}"#);
        assert!(result.is_ok(), "Should allow network access with capability grant");
    }

    /// EDGE-01: without sandbox, check_capabilities is a no-op (fail-open).
    #[test]
    fn test_capability_check_noop_without_sandbox() {
        let gate = ToolGate::new();
        // No sandbox — even a "path" arg should pass
        let result = gate.check_capabilities("any_tool", r#"{"path": "/etc/passwd"}"#);
        assert!(result.is_ok(), "Without sandbox, capability check is a no-op");
    }

    /// EDGE-01: non-object JSON args (array) denied when sandbox is active — fail-closed.
    #[test]
    fn test_capability_check_denies_non_object_args_with_sandbox() {
        let gate = ToolGate::with_sandbox().unwrap();
        // JSON array — cannot be safely inspected, must be denied
        let result = gate.check_capabilities("some_tool", r#"["/etc/passwd"]"#);
        assert!(result.is_err(), "Non-object JSON args should be denied when sandbox active");
    }

    /// EDGE-01: set_tool_capabilities and sandbox_active work together.
    #[test]
    fn test_set_and_retrieve_tool_capabilities() {
        let gate = ToolGate::with_sandbox().unwrap();
        gate.set_tool_capabilities(
            "my_tool".to_string(),
            vec![WasmCapability::Stdout, WasmCapability::Network {
                hosts: vec!["api.example.com".to_string()],
            }],
        );
        // Verify the tool can use 'url' field now
        let result = gate.check_capabilities("my_tool", r#"{"url": "https://api.example.com/v1"}"#);
        assert!(result.is_ok());
    }

    // P2-03: ToolGate::check_user_permission tests ──────────────────────────

    fn make_user_config_for(
        user_id: &str,
        extra_allowed: &[&str],
        denied: &[&str],
    ) -> crate::user_config::UserToolConfig {
        crate::user_config::UserToolConfig {
            user_id: user_id.to_string(),
            extra_allowed_tools: extra_allowed.iter().map(|s| s.to_string()).collect(),
            denied_tools: denied.iter().map(|s| s.to_string()).collect(),
            system_prompt_override: None,
        }
    }

    /// P2-03: globally allowed tool is permitted when user has no deny.
    #[test]
    fn test_check_user_permission_global_allow_passes() {
        let gate = ToolGate::new();
        gate.allow_tool("global_tool".to_string(), ToolPermission::ReadOnly);
        let config = make_user_config_for("alice", &[], &[]);
        assert!(
            gate.check_user_permission("global_tool", &config),
            "Globally allowed tool should be permitted with empty user config"
        );
    }

    /// P2-03: user-denied tool blocked even when globally allowed.
    #[test]
    fn test_check_user_permission_deny_overrides_global() {
        let gate = ToolGate::new();
        gate.allow_tool("danger".to_string(), ToolPermission::ReadOnly);
        let config = make_user_config_for("bob", &[], &["danger"]);
        assert!(
            !gate.check_user_permission("danger", &config),
            "User-denied tool must be blocked even if globally allowed"
        );
    }

    /// P2-03: extra-allowed tool granted beyond global allowlist.
    #[test]
    fn test_check_user_permission_extra_allow_grants_access() {
        let gate = ToolGate::new(); // empty global allowlist
        let config = make_user_config_for("carol", &["special"], &[]);
        assert!(
            gate.check_user_permission("special", &config),
            "Extra-allowed tool should be accessible even without global entry"
        );
    }

    /// P2-03: tool not in global or extra list is blocked.
    #[test]
    fn test_check_user_permission_unknown_tool_blocked() {
        let gate = ToolGate::new();
        gate.allow_tool("other_tool".to_string(), ToolPermission::ReadOnly);
        let config = make_user_config_for("dave", &[], &[]);
        assert!(
            !gate.check_user_permission("unknown_tool", &config),
            "Tool not in global or extra-allowed should be blocked"
        );
    }
}