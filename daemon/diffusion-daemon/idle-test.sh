#!/usr/bin/env bash
# Idle-unload integration test for the dgem daemon. Loads the model, confirms it's resident,
# waits past the idle timeout, and confirms the watcher unloaded it (VRAM released).
set -u
BIN=/opt/lumina/llama-diffusion/build-vulkan/bin/llama-diffusion-daemon
MODEL=/opt/lumina/diffusiongemma-eval/models/diffusiongemma-26B-A4B-it-Q4_K_M.gguf
export LD_LIBRARY_PATH=/opt/lumina/llama-diffusion/build-vulkan/bin
export DGEM_BIND=127.0.0.1 DGEM_HTTP_PORT=8877 DGEM_IDLE_TIMEOUT_SECS=20

pkill -f llama-diffusion-daemon; sleep 2
nohup "$BIN" -m "$MODEL" -ngl 99 -t 4 --diffusion-eb auto -c 8192 -ub 8192 -b 8192 \
  >/tmp/dgem-daemon.log 2>&1 &
sleep 3
mem() { free -m | awk '/Mem:/{print $3" MB used, "$7" MB avail"}'; }
loaded() { curl -s -m 3 http://127.0.0.1:8877/status | python3 -c 'import sys,json;print(json.load(sys.stdin)["model_loaded"])'; }

echo "before-load mem: $(mem)"
curl -s -m 90 http://127.0.0.1:8877/generate -d '{"prompt":"hi","max_tokens":8}' >/dev/null
echo "loaded?         $(loaded)"
echo "loaded mem:     $(mem)"
echo "waiting 30s for idle unload (timeout=20s, watcher tick=5s)..."
sleep 30
echo "post-idle?      $(loaded)            (expect False — VRAM released)"
echo "post-idle mem:  $(mem)"
echo "running still?  $(curl -s -m 3 http://127.0.0.1:8877/health)   (expect ok — listener stays up)"
echo "log unload:     $(grep -i 'unloaded\|released' /tmp/dgem-daemon.log | tail -1)"
echo "=== auto-respawn: next request reloads the model ==="
RESP=$(curl -s -m 90 http://127.0.0.1:8877/generate -d '{"prompt":"hi again","max_tokens":8}')
echo "$RESP" | python3 -c 'import sys,json;d=json.load(sys.stdin);print("reloaded? model_load_ms=",d["model_load_ms"],"(>0 means it respawned the model)")'
echo "after-respawn?  $(loaded)            (expect True)"
