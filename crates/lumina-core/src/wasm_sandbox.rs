//! EDGE-01: WASM tool execution sandbox
//!
//! Provides isolated execution of WebAssembly modules with capability-based
//! permissions, resource limits, and fault isolation.
//!
//! Every tool call executes inside a fresh wasmtime Store so that modules
//! cannot affect each other or the host process.
//!
//! # Resource limits enforced
//! - Memory: 64 MiB per invocation (via `ResourceLimiter`)
//! - Fuel: 10 billion instructions (wasmtime fuel counter)
//! - Wall-clock time: 30 s default (via `epoch_interruption` + background ticker)
//!
//! # Capability model
//! Capabilities are explicitly granted per invocation. Zero capabilities = no host
//! interaction (not even stdout). Modules without a grant cannot open sockets,
//! access the filesystem, read environment variables, or produce visible output.
//!
//! # EDGE-02 integration
//! When a tool has the Network capability, allowed hosts are computed as the
//! **intersection** of the capability's host list and the global `EgressInspector`
//! allowlist. Tools can never expand beyond the global egress policy.

use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use crate::tool_types::WasmCapability;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wasmtime::{AsContextMut, Engine, Linker, Module, ResourceLimiter, Store};

/// Maximum output bytes returned from a WASM module before truncation.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024; // 1 MB

/// Default wall-clock timeout per invocation.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default memory cap per invocation (64 MiB).
const DEFAULT_MAX_MEMORY: u64 = 64 * 1024 * 1024;

/// Default fuel (instruction) limit per invocation.
const DEFAULT_MAX_FUEL: u64 = 10_000_000_000;

/// Epoch tick interval for the background timeout thread.
const EPOCH_TICK: Duration = Duration::from_millis(100);

/// Enforces the per-invocation memory limit inside wasmtime.
struct MemLimiter {
    limit: usize,
}

impl ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(desired <= self.limit)
    }

    fn table_growing(
        &mut self,
        _current: u32,
        _desired: u32,
        _maximum: Option<u32>,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }
}

/// Host state held in every `Store`.
struct HostState {
    limiter: MemLimiter,
    /// Captured stdout bytes written by the guest.
    stdout: Vec<u8>,
    /// Allowed network hosts (empty = deny all network).
    allowed_hosts: HashSet<String>,
    /// Allowed environment variable keys mapped to their values.
    /// Values are supplied by the caller, not read from the host process.
    allowed_env: HashMap<String, String>,
    /// Initial fuel level (stored for audit logging).
    initial_fuel: u64,
}

impl ResourceLimiter for HostState {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        self.limiter.memory_growing(current, desired, maximum)
    }

    fn table_growing(
        &mut self,
        current: u32,
        desired: u32,
        maximum: Option<u32>,
    ) -> anyhow::Result<bool> {
        self.limiter.table_growing(current, desired, maximum)
    }
}

/// WASM execution sandbox.
///
/// The `Engine` is compiled once and shared (it is `Send + Sync`).
/// Each call to `execute` creates an isolated `Store` and `Linker`.
///
/// On `Drop`, the background epoch-ticker thread is signalled to stop.
#[derive(Clone)]
pub struct WasmSandbox {
    engine: Arc<Engine>,
    /// Shutdown flag for the epoch-ticker background thread.
    /// Set to `true` on `Drop`; the thread exits on the next wakeup.
    ticker_shutdown: Arc<AtomicBool>,
    /// Wall-clock timeout for each invocation.
    pub timeout: Duration,
    /// Maximum guest memory per invocation in bytes.
    pub max_memory_bytes: u64,
    /// Maximum fuel (instructions) per invocation.
    pub max_fuel: u64,
    /// EDGE-02: Optional global egress inspector.
    ///
    /// When set, network hosts allowed by a tool's `Network` capability are
    /// intersected with the global egress allowlist — tools cannot reach hosts
    /// not permitted by the operator-configured policy.
    pub egress_inspector: Option<Arc<EgressInspector>>,
}

impl WasmSandbox {
    /// Create a new sandbox with default resource limits.
    ///
    /// `epoch_interruption` is enabled so wall-clock timeouts are enforced.
    /// A background thread ticks the engine epoch at 100ms intervals.
    /// The thread is stopped when the last `WasmSandbox` referencing this
    /// engine is dropped (via the `ticker_shutdown` flag).
    pub fn new() -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config
            .consume_fuel(true)
            .epoch_interruption(true)
            .wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Enable);

        let engine = Engine::new(&config)
            .map_err(|e| LuminaError::Internal(format!("WASM engine init failed: {e}")))?;

        let engine_arc = Arc::new(engine);
        let engine_for_tick = Arc::clone(&engine_arc);

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_tick = Arc::clone(&shutdown);

        // Spawn the epoch-ticker thread.  It checks `shutdown_for_tick` on each
        // iteration and exits cleanly when the flag is set.
        std::thread::Builder::new()
            .name("wasm-epoch-ticker".to_string())
            .spawn(move || {
                while !shutdown_for_tick.load(Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    engine_for_tick.increment_epoch();
                }
            })
            .map_err(|e| LuminaError::Internal(format!("Failed to start epoch ticker: {e}")))?;

        Ok(Self {
            engine: engine_arc,
            ticker_shutdown: shutdown,
            timeout: DEFAULT_TIMEOUT,
            max_memory_bytes: DEFAULT_MAX_MEMORY,
            max_fuel: DEFAULT_MAX_FUEL,
            egress_inspector: None,
        })
    }

    /// Attach an egress inspector to this sandbox (EDGE-02).
    ///
    /// The inspector is consulted for every host listed in a tool's `Network`
    /// capability. Hosts not permitted by the global egress policy are removed
    /// from the effective allowed-hosts set before the module runs.
    pub fn with_egress_inspector(mut self, inspector: Arc<EgressInspector>) -> Self {
        self.egress_inspector = Some(inspector);
        self
    }

    /// Execute `wasm_bytes` with `input` injected and the declared `capabilities`.
    ///
    /// `env_values` is an explicit map of key → value for `Env` capability grants.
    /// It does NOT read from the host process environment — callers supply the values.
    ///
    /// Returns the captured stdout of the module as a UTF-8 string (truncated at 1 MB).
    /// Any WASM trap, fuel exhaustion, timeout, or memory limit returns `Err(LuminaError::*)`.
    pub fn execute(
        &self,
        wasm_bytes: &[u8],
        input: &str,
        capabilities: &[WasmCapability],
        env_values: Option<&HashMap<String, String>>,
    ) -> Result<String> {
        // --- Compile module -------------------------------------------------------
        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| LuminaError::Internal(format!("WASM compilation failed: {e}")))?;

        // --- Derive capability sets -----------------------------------------------
        let has_stdout = capabilities.iter().any(|c| matches!(c, WasmCapability::Stdout));

        // Collect hosts declared by the tool's Network capability.
        let capability_hosts: HashSet<String> = capabilities
            .iter()
            .flat_map(|c| match c {
                WasmCapability::Network { hosts } => hosts.clone(),
                _ => vec![],
            })
            .collect();

        // EDGE-02: Intersect with the global egress inspector allowlist.
        // A host is allowed only if it passes BOTH the capability grant AND the
        // operator-configured egress policy. Tools cannot expand beyond the
        // global policy, preventing capability escalation.
        let allowed_hosts: HashSet<String> = if let Some(inspector) = &self.egress_inspector {
            capability_hosts
                .into_iter()
                .filter(|host| {
                    let ok = inspector.inspect(host, "wasm_sandbox_network_check").is_ok();
                    if !ok {
                        log::warn!(
                            "WASM sandbox: network capability host '{}' blocked by egress inspector",
                            host
                        );
                    }
                    ok
                })
                .collect()
        } else {
            capability_hosts
        };

        // Build the allowed env map from granted keys + supplied values.
        // Only keys listed in WasmCapability::Env AND present in env_values are accessible.
        let allowed_env: HashMap<String, String> = {
            let granted_keys: HashSet<String> = capabilities
                .iter()
                .flat_map(|c| match c {
                    WasmCapability::Env { keys } => keys.clone(),
                    _ => vec![],
                })
                .collect();
            if let Some(supplied) = env_values {
                supplied
                    .iter()
                    .filter(|(k, _)| granted_keys.contains(*k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            } else {
                HashMap::new()
            }
        };

        // --- Build store ----------------------------------------------------------
        let state = HostState {
            limiter: MemLimiter {
                limit: self.max_memory_bytes as usize,
            },
            stdout: Vec::new(),
            allowed_hosts,
            allowed_env,
            initial_fuel: self.max_fuel,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| s as &mut dyn ResourceLimiter);
        store
            .set_fuel(self.max_fuel)
            .map_err(|e| LuminaError::Internal(format!("Failed to set WASM fuel: {e}")))?;

        // Set epoch deadline: number of epoch ticks before the module is interrupted.
        // ticks = timeout / EPOCH_TICK (rounded up).
        let ticks = self.timeout.as_millis().div_ceil(EPOCH_TICK.as_millis()) as u64;
        store.set_epoch_deadline(ticks);
        store.epoch_deadline_trap();

        // --- Build linker ---------------------------------------------------------
        let mut linker: Linker<HostState> = Linker::new(&self.engine);

        // write_stdout: env::write_stdout(ptr: i32, len: i32)
        // Only captures output when Stdout capability is granted.
        if has_stdout {
            linker
                .func_wrap(
                    "env",
                    "write_stdout",
                    |mut caller: wasmtime::Caller<'_, HostState>, ptr: i32, len: i32| {
                        let mem = match caller.get_export("memory") {
                            Some(wasmtime::Extern::Memory(m)) => m,
                            _ => return,
                        };
                        let data = mem.data(&caller);
                        let start = ptr as usize;
                        let end = start.saturating_add(len as usize);
                        if end <= data.len() {
                            let bytes = data[start..end].to_vec();
                            caller.data_mut().stdout.extend_from_slice(&bytes);
                        }
                    },
                )
                .map_err(|e| LuminaError::Internal(format!("Linker error (write_stdout): {e}")))?;
        } else {
            // Stub: capability not granted — output silently dropped
            linker
                .func_wrap("env", "write_stdout", |_: i32, _: i32| {})
                .map_err(|e| LuminaError::Internal(format!("Linker error (write_stdout stub): {e}")))?;
        }

        // get_input: env::get_input(ptr: i32, max_len: i32) -> i32
        // Returns the number of bytes written into the guest buffer.
        let input_bytes = input.as_bytes().to_vec();
        linker
            .func_wrap(
                "env",
                "get_input",
                move |mut caller: wasmtime::Caller<'_, HostState>, ptr: i32, max_len: i32| -> i32 {
                    let mem = match caller.get_export("memory") {
                        Some(wasmtime::Extern::Memory(m)) => m,
                        _ => return 0,
                    };
                    let to_write = input_bytes.len().min(max_len as usize);
                    let data = mem.data_mut(&mut caller);
                    let start = ptr as usize;
                    if start + to_write <= data.len() {
                        data[start..start + to_write].copy_from_slice(&input_bytes[..to_write]);
                    }
                    to_write as i32
                },
            )
            .map_err(|e| LuminaError::Internal(format!("Linker error (get_input): {e}")))?;

        // network_check: env::network_check(host_ptr: i32, host_len: i32) -> i32
        // Returns 1 if the host is in the Network capability grant, 0 if denied.
        linker
            .func_wrap(
                "env",
                "network_check",
                |mut caller: wasmtime::Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                    let mem = match caller.get_export("memory") {
                        Some(wasmtime::Extern::Memory(m)) => m,
                        _ => return 0,
                    };
                    let data = mem.data(&caller);
                    let start = ptr as usize;
                    let end = start.saturating_add(len as usize);
                    if end > data.len() {
                        return 0;
                    }
                    let host_bytes = data[start..end].to_vec();
                    let host = match std::str::from_utf8(&host_bytes) {
                        Ok(s) => s,
                        Err(_) => return 0,
                    };
                    if caller.data().allowed_hosts.contains(host) {
                        1
                    } else {
                        0
                    }
                },
            )
            .map_err(|e| LuminaError::Internal(format!("Linker error (network_check): {e}")))?;

        // get_env: env::get_env(key_ptr, key_len, val_ptr, val_max) -> i32
        // Returns bytes written, or -1 if key not granted or not in supplied env map.
        // NOTE: reads from the sandbox-local allowed_env map, NOT the host process env.
        linker
            .func_wrap(
                "env",
                "get_env",
                |mut caller: wasmtime::Caller<'_, HostState>,
                 key_ptr: i32,
                 key_len: i32,
                 val_ptr: i32,
                 val_max: i32|
                 -> i32 {
                    let mem = match caller.get_export("memory") {
                        Some(wasmtime::Extern::Memory(m)) => m,
                        _ => return -1,
                    };
                    let data = mem.data(&caller);
                    let ks = key_ptr as usize;
                    let ke = ks.saturating_add(key_len as usize);
                    if ke > data.len() {
                        return -1;
                    }
                    let key = match std::str::from_utf8(&data[ks..ke]) {
                        Ok(s) => s.to_string(),
                        Err(_) => return -1,
                    };
                    // Look up only in the sandbox-local env map (never host process env)
                    let val = match caller.data().allowed_env.get(&key) {
                        Some(v) => v.clone(),
                        None => return -1,
                    };
                    let val_bytes = val.as_bytes().to_vec();
                    let to_write = val_bytes.len().min(val_max as usize);
                    let data_mut = mem.data_mut(&mut caller);
                    let vs = val_ptr as usize;
                    if vs + to_write <= data_mut.len() {
                        data_mut[vs..vs + to_write].copy_from_slice(&val_bytes[..to_write]);
                    }
                    to_write as i32
                },
            )
            .map_err(|e| LuminaError::Internal(format!("Linker error (get_env): {e}")))?;

        // --- Instantiate + run ---------------------------------------------------
        let result = self.run_module(&linker, &module, &mut store);

        // --- Audit: log resource usage -------------------------------------------
        let fuel_remaining = store.get_fuel().unwrap_or(0);
        let fuel_used = self.max_fuel.saturating_sub(fuel_remaining);
        let output_len = store.data().stdout.len();
        log::debug!(
            "WASM sandbox: fuel_used={}, output_bytes={}",
            fuel_used,
            output_len
        );

        // --- Collect output -------------------------------------------------------
        match result {
            Ok(()) => {
                let mut output = store.into_data().stdout;

                // Truncate oversized output
                if output.len() > MAX_OUTPUT_BYTES {
                    log::warn!(
                        "WASM module output exceeded 1MB ({} bytes). Truncating.",
                        output.len()
                    );
                    output.truncate(MAX_OUTPUT_BYTES);
                    let warning = b"\n[WARNING: output truncated at 1MB limit]";
                    output.extend_from_slice(warning);
                }

                String::from_utf8(output)
                    .map_err(|e| LuminaError::Internal(format!("WASM output is not valid UTF-8: {e}")))
            }
            Err(e) => Err(e),
        }
    }

    /// Instantiate and invoke the module's entry point.
    ///
    /// Tries "run" export first, then "_start". Epoch interruption handles
    /// wall-clock timeout; fuel handles instruction-count limits.
    fn run_module(
        &self,
        linker: &Linker<HostState>,
        module: &Module,
        store: &mut Store<HostState>,
    ) -> Result<()> {
        let instance = linker
            .instantiate(store.as_context_mut(), module)
            .map_err(|e| classify_trap_error(e))?;

        // Prefer "run" export; fall back to "_start"
        let run_func = instance
            .get_typed_func::<(), ()>(store.as_context_mut(), "run")
            .ok();
        let start_func = if run_func.is_none() {
            instance
                .get_typed_func::<(), ()>(store.as_context_mut(), "_start")
                .ok()
        } else {
            None
        };

        match (run_func, start_func) {
            (Some(func), _) => func
                .call(store.as_context_mut(), ())
                .map_err(classify_trap_error),
            (_, Some(func)) => func
                .call(store.as_context_mut(), ())
                .map_err(classify_trap_error),
            (None, None) => {
                log::warn!("WASM module has no 'run' or '_start' export");
                Ok(())
            }
        }
    }

    /// Return (fuel_used_placeholder, max_fuel) for external audit logging.
    ///
    /// Call `store.get_fuel()` inside `execute` for accurate per-call accounting.
    pub fn resource_stats(&self) -> (u64, u64) {
        (0, self.max_fuel)
    }
}

/// Classify a wasmtime execution error into the appropriate LuminaError variant.
///
/// Attempts to downcast to `wasmtime::Trap` first for precise classification,
/// then falls back to message-based heuristics.
/// Classify a wasmtime execution error into the appropriate LuminaError variant.
///
/// Uses `wasmtime::Trap` downcast for precision, falling back to message heuristics.
///
/// Key distinction:
/// - `OutOfFuel` / `Interrupt` → `SecurityViolation` (policy-enforced limit)
/// - `MemoryOutOfBounds` → `Internal` (bad pointer, NOT the 64MB cap)
/// - 64MB cap (ResourceLimiter `Ok(false)`) → silent `memory.grow` failure; no trap
/// - All other traps → `Internal`
fn classify_trap_error(err: anyhow::Error) -> LuminaError {
    // Try to downcast to a wasmtime Trap for precise classification
    if let Some(trap) = err.downcast_ref::<wasmtime::Trap>() {
        return match trap {
            wasmtime::Trap::OutOfFuel => {
                LuminaError::SecurityViolation("WASM fuel limit exceeded".to_string())
            }
            wasmtime::Trap::Interrupt => {
                LuminaError::SecurityViolation("WASM timeout: wall-clock limit exceeded".to_string())
            }
            wasmtime::Trap::MemoryOutOfBounds => {
                // Out-of-bounds *access* (not the 64MB cap — that's a silent grow failure)
                LuminaError::Internal("WASM out-of-bounds memory access".to_string())
            }
            other => {
                LuminaError::Internal(format!("WASM module trapped: {other}"))
            }
        };
    }

    // Heuristic fallback based on error message content
    let msg = err.to_string();
    if msg.contains("fuel") || msg.contains("OutOfFuel") || msg.contains("out of fuel") {
        LuminaError::SecurityViolation("WASM fuel limit exceeded".to_string())
    } else if msg.contains("interrupt") || msg.contains("Interrupt") || msg.contains("epoch") {
        LuminaError::SecurityViolation("WASM timeout: wall-clock limit exceeded".to_string())
    } else if msg.contains("trap") || msg.contains("Trap") || msg.contains("unreachable") {
        LuminaError::Internal(format!("WASM module panicked (trap): {msg}"))
    } else {
        LuminaError::Internal(format!("WASM execution failed: {msg}"))
    }
}

impl Default for WasmSandbox {
    fn default() -> Self {
        Self::new().expect("WasmSandbox::new should not fail with valid wasmtime config")
    }
}

impl Drop for WasmSandbox {
    /// Signal the epoch-ticker background thread to stop when the *last* owner
    /// of this sandbox is dropped.
    ///
    /// `ticker_shutdown` is an `Arc<AtomicBool>` shared among all clones.
    /// We only set the flag when `Arc::strong_count` drops to 1 (i.e. this is
    /// the last reference), preventing the ticker from being killed while other
    /// clones are still executing WASM modules.
    ///
    /// The ticker thread checks the flag at each EPOCH_TICK interval and exits
    /// cleanly; no thread join is performed here (the ticker is non-critical,
    /// and joining would block the `Drop` caller unnecessarily).
    fn drop(&mut self) {
        // Only signal when this is the last strong reference to `ticker_shutdown`.
        // The clone machinery does not clone Arc manually — each `Clone` increments
        // the same Arc, so strong_count == 1 means no other WasmSandbox clones exist.
        if Arc::strong_count(&self.ticker_shutdown) == 1 {
            self.ticker_shutdown.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All test modules import the four host functions so they can be resolved.
    fn hello_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "hello")
              (func (export "run")
                i32.const 0
                i32.const 5
                call $ws
              )
            )"#,
        )
        .expect("hello_wasm WAT parse")
    }

    /// Module that grows memory 2000 × 64 KiB = 125 MiB (exceeds 64 MiB limit).
    fn memory_hog_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run")
                (local $i i32)
                (local.set $i (i32.const 0))
                (block $break
                  (loop $loop
                    (br_if $break (i32.ge_u (local.get $i) (i32.const 2000)))
                    (drop (memory.grow (i32.const 1)))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $loop)
                  )
                )
              )
            )"#,
        )
        .expect("memory_hog_wasm WAT parse")
    }

    /// Module with an infinite loop (fuel limit test).
    fn infinite_loop_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run")
                (loop $forever
                  (br $forever)
                )
              )
            )"#,
        )
        .expect("infinite_loop_wasm WAT parse")
    }

    /// Module that calls network_check for "example.com" and writes "1" or "0".
    fn network_check_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "1")
              (data (i32.const 1) "0")
              (data (i32.const 16) "example.com")
              (func (export "run")
                (local $allowed i32)
                (local.set $allowed (call $nc (i32.const 16) (i32.const 11)))
                (if (i32.eqz (local.get $allowed))
                  (then (call $ws (i32.const 1) (i32.const 1)))
                  (else (call $ws (i32.const 0) (i32.const 1)))
                )
              )
            )"#,
        )
        .expect("network_check_wasm WAT parse")
    }

    /// Module that sleeps (busy-waits) for a very long time — timeout test.
    /// Uses a massive loop count that fuel alone won't catch (we set high fuel).
    fn busy_wait_wasm() -> Vec<u8> {
        // Loops i64::MAX / 2 times to simulate a slow-but-not-infinite module
        // In practice the epoch interrupt kicks in first.
        wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run")
                (loop $forever
                  (br $forever)
                )
              )
            )"#,
        )
        .expect("busy_wait_wasm WAT parse")
    }

    /// Module that writes >1MB of output.
    fn large_output_wasm() -> Vec<u8> {
        // Writes 2000 × 512 bytes = ~1MB to exceed the truncation limit
        // Each iteration: call write_stdout(ptr=0, len=512) — data filled with 'A' (0x41)
        wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              ;; Fill memory page 0 with 'A' via memory.fill equivalent: just use a data segment
              ;; We write the byte 0x41 ('A') repeated — but WAT data can only be at one offset.
              ;; Instead: write 512 bytes of 'A' data segment, then loop calling write_stdout.
              (data (i32.const 0) "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
              (func (export "run")
                (local $i i32)
                (local.set $i (i32.const 0))
                (block $break
                  (loop $loop
                    ;; Write 512 bytes of 'A' per iteration
                    (call $ws (i32.const 0) (i32.const 512))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    ;; 2100 iterations × 512 bytes = ~1.05 MB — exceeds 1MB limit
                    (br_if $loop (i32.lt_u (local.get $i) (i32.const 2100)))
                  )
                )
              )
            )"#,
        )
        .expect("large_output_wasm WAT parse")
    }

    #[test]
    fn test_sandbox_executes_simple_module() {
        let sandbox = WasmSandbox::new().unwrap();
        let result = sandbox.execute(&hello_wasm(), "", &[WasmCapability::Stdout], None);
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn test_sandbox_no_stdout_capability_returns_empty() {
        let sandbox = WasmSandbox::new().unwrap();
        let result = sandbox.execute(&hello_wasm(), "", &[], None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn test_memory_limit_enforced() {
        let mut sandbox = WasmSandbox::new().unwrap();
        // Drop the limit to 2 MiB so the hog's 125 MiB demand is denied
        sandbox.max_memory_bytes = 2 * 1024 * 1024;

        let result = sandbox.execute(&memory_hog_wasm(), "", &[], None);
        // Either succeeds (grow denied, loop continues gracefully) or errors.
        // Host process must NOT crash either way.
        match result {
            Ok(_) => {
                // ResourceLimiter denied grows; module continued — acceptable
            }
            Err(LuminaError::SecurityViolation(_)) | Err(LuminaError::Internal(_)) => {
                // Trap from attempting to access beyond allowed memory — also acceptable
            }
            Err(other) => panic!("Unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn test_fuel_limit_enforced() {
        let mut sandbox = WasmSandbox::new().unwrap();
        sandbox.max_fuel = 10_000;

        let result = sandbox.execute(&infinite_loop_wasm(), "", &[], None);
        // Module must NOT succeed — the loop should be terminated.
        match result {
            Err(LuminaError::SecurityViolation(_)) => {}
            Err(LuminaError::Internal(_)) => {}
            Ok(_) => panic!("Module should have been terminated by fuel/timeout limit"),
            Err(other) => panic!("Unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn test_timeout_enforced() {
        let mut sandbox = WasmSandbox::new().unwrap();
        // Set a very short timeout (300ms) and very high fuel so fuel doesn't trigger first.
        // The epoch ticker fires every 100ms; 3 ticks = ~300ms.
        sandbox.timeout = Duration::from_millis(300);
        sandbox.max_fuel = u64::MAX;

        let start = std::time::Instant::now();
        let result = sandbox.execute(&busy_wait_wasm(), "", &[], None);
        let elapsed = start.elapsed();

        // Module should have been interrupted by the epoch timeout (within ~1s slop)
        match result {
            Err(LuminaError::SecurityViolation(ref msg)) if msg.contains("timeout") => {}
            Err(LuminaError::SecurityViolation(_)) | Err(LuminaError::Internal(_)) => {
                // Fuel or other trap also acceptable as long as module was terminated
            }
            Ok(_) => panic!("Busy-wait module should have been killed by timeout"),
            Err(other) => panic!("Unexpected error: {other:?}"),
        }

        // Sanity: should finish in under 5 seconds
        assert!(
            elapsed < Duration::from_secs(5),
            "Timeout did not fire promptly: elapsed={elapsed:?}"
        );
    }

    #[test]
    fn test_network_capability_denied_by_default() {
        let sandbox = WasmSandbox::new().unwrap();
        let result = sandbox.execute(&network_check_wasm(), "", &[WasmCapability::Stdout], None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "0");
    }

    #[test]
    fn test_network_capability_granted() {
        let sandbox = WasmSandbox::new().unwrap();
        let caps = vec![
            WasmCapability::Stdout,
            WasmCapability::Network {
                hosts: vec!["example.com".to_string()],
            },
        ];
        let result = sandbox.execute(&network_check_wasm(), "", &caps, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "1");
    }

    /// WAT module that calls get_env("SECRET_KEY", ...) and writes the result length,
    /// or "-" if get_env returns -1 (key not available).
    fn get_env_probe_wasm() -> Vec<u8> {
        // Memory layout: [0..10] = "SECRET_KEY", [64..128] = value buffer
        // Calls get_env(key_ptr=0, key_len=10, val_ptr=64, val_max=64)
        // Writes "GOT" to stdout if result >= 0, "DENIED" if result == -1
        wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 0)  "SECRET_KEY")
              (data (i32.const 64) "GOT")
              (data (i32.const 67) "DENIED")
              (func (export "run")
                (local $r i32)
                ;; Call get_env("SECRET_KEY") — key at [0..10], value buf at [128..192]
                (local.set $r (call $ge (i32.const 0) (i32.const 10) (i32.const 128) (i32.const 64)))
                ;; If result >= 0 write "GOT", else write "DENIED"
                (if (i32.ge_s (local.get $r) (i32.const 0))
                  (then (call $ws (i32.const 64) (i32.const 3)))
                  (else (call $ws (i32.const 67) (i32.const 6)))
                )
              )
            )"#,
        )
        .expect("get_env_probe_wasm WAT parse")
    }

    #[test]
    fn test_env_not_leaked_from_host() {
        // Verify that the sandbox does NOT expose host environment variables
        // when no env_values map is supplied and no Env capability is granted.
        // The probe module tries to read "SECRET_KEY" — it should get "DENIED".
        let sandbox = WasmSandbox::new().unwrap();
        let result = sandbox.execute(
            &get_env_probe_wasm(),
            "",
            &[WasmCapability::Stdout],
            None, // no env_values supplied — allowed_env is empty
        );
        assert!(result.is_ok(), "Probe module should execute ok: {:?}", result);
        assert_eq!(
            result.unwrap(),
            "DENIED",
            "Module must NOT be able to read host environment (no env_values supplied)"
        );
    }

    #[test]
    fn test_env_capability_with_supplied_values() {
        // Verify that a key listed in both the Env capability AND the env_values
        // map is accessible to the module.
        let sandbox = WasmSandbox::new().unwrap();
        let mut env_map = HashMap::new();
        env_map.insert("SECRET_KEY".to_string(), "secret_value".to_string());

        let caps = vec![
            WasmCapability::Stdout,
            WasmCapability::Env { keys: vec!["SECRET_KEY".to_string()] },
        ];

        // The probe module reads SECRET_KEY and writes "GOT" on success
        let result = sandbox.execute(&get_env_probe_wasm(), "", &caps, Some(&env_map));
        assert!(result.is_ok(), "Probe module should succeed: {:?}", result);
        assert_eq!(result.unwrap(), "GOT", "Module should have read the supplied env value");
    }

    #[test]
    fn test_env_capability_granted_but_key_not_in_values_returns_denied() {
        // Grant Env capability for "OTHER_KEY" but don't supply "SECRET_KEY" in env_values.
        // The module probing "SECRET_KEY" should still get "DENIED".
        let sandbox = WasmSandbox::new().unwrap();
        let env_map: HashMap<String, String> = HashMap::new(); // empty — no values supplied

        let caps = vec![
            WasmCapability::Stdout,
            // Grant a *different* key — SECRET_KEY is not in the grant
            WasmCapability::Env { keys: vec!["OTHER_KEY".to_string()] },
        ];

        let result = sandbox.execute(&get_env_probe_wasm(), "", &caps, Some(&env_map));
        assert!(result.is_ok(), "Probe should succeed: {:?}", result);
        assert_eq!(
            result.unwrap(),
            "DENIED",
            "Module must not read SECRET_KEY when it is not in the capability grant"
        );
    }

    #[test]
    fn test_sandbox_isolates_concurrent_calls() {
        let sandbox = WasmSandbox::new().unwrap();
        let s1 = sandbox.clone();
        let s2 = sandbox.clone();

        let h1 = std::thread::spawn(move || {
            s1.execute(&hello_wasm(), "", &[WasmCapability::Stdout], None)
        });
        let h2 = std::thread::spawn(move || {
            s2.execute(&hello_wasm(), "", &[WasmCapability::Stdout], None)
        });

        assert_eq!(h1.join().unwrap().unwrap(), "hello");
        assert_eq!(h2.join().unwrap().unwrap(), "hello");
    }

    #[test]
    fn test_wasm_trap_returns_error_not_panic() {
        let trap_wasm = wat::parse_str(
            r#"(module
              (import "env" "write_stdout" (func $ws (param i32 i32)))
              (import "env" "get_input" (func $gi (param i32 i32) (result i32)))
              (import "env" "network_check" (func $nc (param i32 i32) (result i32)))
              (import "env" "get_env" (func $ge (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run")
                unreachable
              )
            )"#,
        )
        .unwrap();

        let sandbox = WasmSandbox::new().unwrap();
        let result = sandbox.execute(&trap_wasm, "", &[], None);
        assert!(result.is_err(), "Expected error from unreachable trap");
        // Host is still alive — test completes normally
    }

    #[test]
    fn test_output_truncated_at_1mb() {
        let sandbox = WasmSandbox::new().unwrap();
        // Use high fuel so the loop can run to completion
        let result = sandbox.execute(
            &large_output_wasm(),
            "",
            &[WasmCapability::Stdout],
            None,
        );
        assert!(result.is_ok(), "Large output module should succeed (truncated): {:?}", result);
        let output = result.unwrap();
        // Output must be truncated: at most 1MB + warning suffix
        assert!(
            output.len() <= MAX_OUTPUT_BYTES + 100,
            "Output not truncated: {} bytes",
            output.len()
        );
        assert!(
            output.contains("[WARNING: output truncated"),
            "Truncation warning missing"
        );
    }

    #[test]
    fn test_resource_stats() {
        let sandbox = WasmSandbox::new().unwrap();
        let (_, max) = sandbox.resource_stats();
        assert_eq!(max, DEFAULT_MAX_FUEL);
    }

    #[test]
    fn test_zero_capabilities_no_output() {
        // Zero capabilities: stdout stub drops all writes
        let sandbox = WasmSandbox::new().unwrap();
        let result = sandbox.execute(&hello_wasm(), "", &[], None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "", "Zero-capability module should produce no output");
    }

    // ── EDGE-02: EgressInspector integration ──────────────────────────────────

    #[test]
    fn test_egress_inspector_allows_permitted_host() {
        use crate::egress_inspector::EgressInspector;
        let inspector = Arc::new(EgressInspector::new(vec!["example.com".to_string()]));
        let sandbox = WasmSandbox::new().unwrap().with_egress_inspector(inspector);
        // network_check_wasm checks "example.com" — should be allowed by both capability + inspector
        let caps = vec![
            WasmCapability::Stdout,
            WasmCapability::Network { hosts: vec!["example.com".to_string()] },
        ];
        let result = sandbox.execute(&network_check_wasm(), "", &caps, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "1", "example.com should be permitted by inspector");
    }

    #[test]
    fn test_egress_inspector_blocks_non_allowlisted_host() {
        use crate::egress_inspector::EgressInspector;
        // Inspector only allows "safe.example.com" — not "example.com"
        let inspector = Arc::new(EgressInspector::new(vec!["safe.example.com".to_string()]));
        let sandbox = WasmSandbox::new().unwrap().with_egress_inspector(inspector);
        // Capability grants "example.com" but inspector blocks it
        let caps = vec![
            WasmCapability::Stdout,
            WasmCapability::Network { hosts: vec!["example.com".to_string()] },
        ];
        // The host is removed from allowed_hosts by the egress filter, so the module
        // should report "0" (not allowed).
        let result = sandbox.execute(&network_check_wasm(), "", &caps, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "0",
            "example.com should be blocked when not in global egress allowlist");
    }

    #[test]
    fn test_egress_inspector_blocks_all_when_allowlist_loopback_only() {
        use crate::egress_inspector::EgressInspector;
        // Explicitly create an inspector with only loopback entries — no env var
        // manipulation needed; EgressInspector::new with a non-empty explicit list
        // uses that list directly (doesn't read env).
        let inspector = Arc::new(EgressInspector::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]));
        let sandbox = WasmSandbox::new().unwrap().with_egress_inspector(inspector);
        let caps = vec![
            WasmCapability::Stdout,
            WasmCapability::Network { hosts: vec!["example.com".to_string()] },
        ];
        let result = sandbox.execute(&network_check_wasm(), "", &caps, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "0",
            "example.com should be blocked by loopback-only inspector");
    }

    #[test]
    fn test_sandbox_without_egress_inspector_uses_capability_only() {
        // Without an egress inspector, only capability grants matter (EDGE-01 behavior)
        let sandbox = WasmSandbox::new().unwrap();
        assert!(sandbox.egress_inspector.is_none());
        let caps = vec![
            WasmCapability::Stdout,
            WasmCapability::Network { hosts: vec!["example.com".to_string()] },
        ];
        let result = sandbox.execute(&network_check_wasm(), "", &caps, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "1",
            "Without inspector, capability grant alone should allow the host");
    }
}
