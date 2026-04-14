#!/usr/bin/env python3
"""
Matrix-IronClaw Bridge Bot v3
Bridges Lumina's Matrix account to IronClaw's agent gateway API.

Architecture:
  Element (phone/web) → Matrix (Tuwunel) → this bot → IronClaw gateway API
  IronClaw gateway SSE events → this bot → Matrix room

Features:
  - Full agent loop (MCP tools, identity, routines, memory)
  - Auto-approval of tool calls triggered by user messages
  - Manual approval flow for unsolicited tool calls (routines)
  - Persistent thread ID (survives restarts via .thread file)
  - Formatted Matrix messages (markdown rendering in Element)
  - Auto-accept room invites
  - Exponential backoff on SSE reconnect
  - Multi-room capable (responds in any joined room)

Config: /opt/matrix-bridge/.env (populated by fetch-secrets.sh)
Thread: /opt/matrix-bridge/.thread (auto-created, persists across restarts)
"""

import asyncio
import json
import logging
import re
import sys
from pathlib import Path

import aiohttp
from nio import (
    AsyncClient,
    LoginResponse,
    MatrixRoom,
    RoomMessageText,
    InviteMemberEvent,
)

# ── Logging ──

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
log = logging.getLogger("bridge")

# ── Config ──

BASE_DIR = Path(__file__).parent


def load_env():
    env = {}
    env_file = BASE_DIR / ".env"
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
    return env


cfg = load_env()

HOMESERVER = cfg.get("MATRIX_HOMESERVER", "http://localhost:6167")
MATRIX_USER = cfg.get("MATRIX_USER", "")
MATRIX_PASSWORD = cfg.get("MATRIX_PASSWORD", "")
ROOM_ID = cfg.get("MATRIX_ROOM_ID", "")
GATEWAY_URL = cfg.get("IRONCLAW_GATEWAY_URL", "").rstrip("/")
GATEWAY_TOKEN = cfg.get("IRONCLAW_GATEWAY_TOKEN", "")

# Derive base URL (strip /v1/chat/completions if present)
BASE_URL = GATEWAY_URL.split("/v1/")[0] if "/v1/" in GATEWAY_URL else GATEWAY_URL

# Build full Matrix user ID for filtering own messages
# Handles both "http://localhost:6167" and "https://matrix.moosenet.online"
# Server name for Matrix IDs ��� this is the federation domain, not the homeserver URL
# For Tuwunel at localhost:6167 serving moosenet.online, the domain is moosenet.online
# MATRIX_USER from .env may be 'lumina' or '@lumina:moosenet.online' ��� normalize
if MATRIX_USER.startswith('@'):
    MATRIX_USER_ID = MATRIX_USER  # already full format
else:
    MATRIX_USER_ID = f"@{MATRIX_USER}:moosenet.online"

THREAD_FILE = BASE_DIR / ".thread"
MAX_MESSAGE_LEN = 4000
AGENT_TIMEOUT = 300  # seconds


# ── State ──

class BridgeState:
    """Centralized mutable state."""
    def __init__(self):
        self.thread_id = ""
        self.client = None
        self.startup_complete = False
        self.waiting_for_response = False
        self.pending_approval = None
        self.sse_queue = asyncio.Queue()
        # Fix: serialization lock prevents concurrent message handling (ordering bug)
        self.processing_lock = asyncio.Lock()
        # Fix: track seen event IDs to prevent duplicate SSE replay
        self.seen_event_ids: set = set()
        # Fix: track last response content to deduplicate identical responses
        self.last_response_content: str = ""

    def load_thread(self):
        if THREAD_FILE.exists():
            tid = THREAD_FILE.read_text().strip()
            if tid:
                self.thread_id = tid
                log.info(f"Loaded persisted thread: {tid}")

    def save_thread(self):
        if self.thread_id:
            THREAD_FILE.write_text(self.thread_id)
            log.info(f"Persisted thread to {THREAD_FILE}")


state = BridgeState()


# ── IronClaw Gateway ──

def _headers():
    return {
        "Authorization": f"Bearer {GATEWAY_TOKEN}",
        "Content-Type": "application/json",
    }


async def ensure_thread():
    """Load existing thread or create a new one."""
    state.load_thread()
    if state.thread_id:
        return

    async with aiohttp.ClientSession() as session:
        # Search for existing matrix-bridge thread
        try:
            async with session.get(f"{BASE_URL}/api/chat/threads", headers=_headers()) as resp:
                if resp.status in (200, 202):
                    data = await resp.json()
                    for t in data.get("threads", []):
                        if t.get("title") == "matrix-bridge" and t.get("thread_type") == "thread":
                            state.thread_id = t["id"]
                            log.info(f"Found existing thread: {state.thread_id}")
                            state.save_thread()
                            return
        except Exception as e:
            log.warning(f"Could not list threads: {e}")

        # Create new thread
        try:
            async with session.post(f"{BASE_URL}/api/chat/thread/new", headers=_headers()) as resp:
                if resp.status in (200, 202):
                    data = await resp.json()
                    state.thread_id = data["id"]
                    log.info(f"Created new thread: {state.thread_id}")
                    state.save_thread()
                else:
                    body = await resp.text()
                    log.error(f"Thread creation failed: {resp.status} {body}")
                    sys.exit(1)
        except Exception as e:
            log.error(f"Thread creation error: {e}")
            sys.exit(1)


async def approve_tool(request_id, thread_id):
    """Send tool approval to the gateway."""
    payload = {"request_id": request_id, "action": "approve", "thread_id": thread_id}
    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(f"{BASE_URL}/api/chat/approval", json=payload, headers=_headers()) as resp:
                if resp.status in (200, 202):
                    log.info(f"Approved: {request_id}")
                else:
                    body = await resp.text()
                    log.error(f"Approve failed: {resp.status} {body}")
    except Exception as e:
        log.error(f"Approve error: {e}")


async def send_to_ironclaw(message):
    """Send message to IronClaw agent and wait for SSE response."""
    payload = {
        "content": message,
        "thread_id": state.thread_id,
        "timezone": "America/Los_Angeles",
    }

    # Drain stale events
    while not state.sse_queue.empty():
        try:
            state.sse_queue.get_nowait()
        except asyncio.QueueEmpty:
            break

    state.waiting_for_response = True

    try:
        async with aiohttp.ClientSession() as session:
            async with session.post(f"{BASE_URL}/api/chat/send", json=payload, headers=_headers()) as resp:
                if resp.status not in (200, 202):
                    body = await resp.text()
                    log.error(f"Send failed: {resp.status} {body}")
                    return f"Error: Send failed ({resp.status})"
                data = await resp.json()
                log.info(f"Message accepted: {data.get('message_id', '?')}")

        response = await asyncio.wait_for(state.sse_queue.get(), timeout=AGENT_TIMEOUT)
        return response

    except asyncio.TimeoutError:
        log.error(f"Timed out waiting for agent response ({AGENT_TIMEOUT}s)")
        return "Error: Agent response timed out"
    except Exception as e:
        log.error(f"Gateway error: {e}")
        return f"Error: {e}"
    finally:
        state.waiting_for_response = False


# ── SSE Listener ──

async def sse_listener():
    """Persistent SSE connection with exponential backoff."""
    url = f"{BASE_URL}/api/chat/events?token={GATEWAY_TOKEN}"
    backoff = 2

    while True:
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get(url, timeout=aiohttp.ClientTimeout(total=0)) as resp:
                    log.info("SSE connected")
                    backoff = 2
                    buffer = ""
                    event_type = ""

                    async for chunk in resp.content:
                        text = chunk.decode("utf-8", errors="replace")
                        buffer += text

                        while "\n" in buffer:
                            line, buffer = buffer.split("\n", 1)
                            line = line.strip()

                            if line.startswith("event:"):
                                event_type = line[6:].strip()
                            elif line.startswith("data:"):
                                await _handle_sse_event(event_type, line[5:].strip())
                            elif line == "":
                                event_type = ""

        except asyncio.CancelledError:
            log.info("SSE listener cancelled")
            return
        except Exception as e:
            log.warning(f"SSE lost: {e}. Reconnecting in {backoff}s...")
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 60)


async def _handle_sse_event(event_type, data_str):
    """Process a single SSE event."""
    try:
        data = json.loads(data_str)
    except json.JSONDecodeError:
        return

    if event_type == "response":
        content = data.get("content", "")
        if content:
            # Fix: deduplicate identical consecutive responses (IronClaw sends response events multiple times)
            if content == state.last_response_content:
                log.info(f"SSE response DUPLICATE skipped ({len(content)} chars)")
                return
            state.last_response_content = content
            # Fix: drain any queued responses before putting new one (prevents ordering mismatch)
            while not state.sse_queue.empty():
                try:
                    state.sse_queue.get_nowait()
                    log.info("SSE queue: drained stale response")
                except asyncio.QueueEmpty:
                    break
            await state.sse_queue.put(content)
            log.info(f"SSE response: {len(content)} chars - content: {content[:500]}")

    elif event_type == "thinking":
        msg = data.get("message", "")
        if msg:
            log.info(f"Agent: {msg}")

    elif event_type == "status":
        log.info(f"Status: {data.get('message', '')}")

    elif event_type == "turn_cost":
        log.info(f"Turn cost: ${data.get('cost_usd', '?')}")

    elif event_type == "approval_needed":
        request_id = data.get("request_id", "")
        tool_name = data.get("tool_name", "unknown")
        thread_id = data.get("thread_id", "")
        log.info(f"Approval needed: {tool_name} ({request_id})")

        if state.waiting_for_response and thread_id == state.thread_id:
            log.info(f"Auto-approving {tool_name}")
            asyncio.create_task(approve_tool(request_id, thread_id))
        else:
            state.pending_approval = {
                "request_id": request_id,
                "thread_id": thread_id,
                "tool_name": tool_name,
            }
            await post_to_matrix(
                f"🔒 **Approval needed:** {tool_name}\n\nReply **approve** or **deny**."
            )


# ── Matrix ──

def _markdown_to_html(text):
    """Minimal markdown → HTML for Matrix formatted messages."""
    html = text
    html = re.sub(r"```(.+?)```", r"<pre><code>\1</code></pre>", html, flags=re.DOTALL)
    html = re.sub(r"`(.+?)`", r"<code>\1</code>", html)
    html = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", html)
    html = re.sub(r"\*(.+?)\*", r"<em>\1</em>", html)
    html = re.sub(r"^### (.+)$", r"<h3>\1</h3>", html, flags=re.MULTILINE)
    html = re.sub(r"^## (.+)$", r"<h2>\1</h2>", html, flags=re.MULTILINE)
    html = re.sub(r"^# (.+)$", r"<h1>\1</h1>", html, flags=re.MULTILINE)
    html = html.replace("\n", "<br>")
    return html


async def post_to_matrix(text, room_id=None):
    """Send a formatted message to a Matrix room."""
    if not state.client:
        return
    target = room_id or ROOM_ID

    await state.client.room_send(
        room_id=target,
        message_type="m.room.message",
        content={
            "msgtype": "m.text",
            "body": text,
            "format": "org.matrix.custom.html",
            "formatted_body": _markdown_to_html(text),
        },
    )


async def message_callback(room, event):
    """Handle incoming Matrix messages from the operator."""
    if not state.startup_complete:
        return

    # Ignore own messages
    if event.sender == MATRIX_USER_ID:
        return
    if MATRIX_USER and event.sender.startswith(f"@{MATRIX_USER}:"):
        return

    user_message = event.body.strip()
    if not user_message:
        return

    # Fix: deduplicate events by Matrix event ID (prevents replay duplicates on reconnect)
    event_id = getattr(event, 'event_id', None)
    if event_id:
        if event_id in state.seen_event_ids:
            log.info(f"Duplicate event {event_id[:16]} skipped")
            return
        state.seen_event_ids.add(event_id)
        # Keep set bounded
        if len(state.seen_event_ids) > 200:
            state.seen_event_ids = set(list(state.seen_event_ids)[-100:])

    log.info(f"[{room.display_name}] {event.sender}: {user_message[:80]}...")

    # Fix: serialization lock — process one message at a time to preserve ordering
    async with state.processing_lock:
        # Handle approval responses
        if state.pending_approval:
            lower = user_message.lower()
            if lower in ("approve", "approved", "yes", "ok"):
                tool = state.pending_approval["tool_name"]
                log.info(f"Manual approval: {tool}")
                await approve_tool(state.pending_approval["request_id"], state.pending_approval["thread_id"])
                await post_to_matrix(f"✅ Approved: **{tool}**", room.room_id)
                state.pending_approval = None
                return
            elif lower in ("deny", "denied", "no", "reject"):
                tool = state.pending_approval["tool_name"]
                log.info(f"Manual denial: {tool}")
                state.pending_approval = None
                await post_to_matrix(f"❌ Denied: **{tool}**", room.room_id)
                return

        # Show typing indicator
        await state.client.room_typing(room.room_id, typing_state=True)

        response = await send_to_ironclaw(user_message)

        await state.client.room_typing(room.room_id, typing_state=False)

        # Send response (chunked if needed)
        if len(response) > MAX_MESSAGE_LEN:
            chunks = [response[i:i + MAX_MESSAGE_LEN] for i in range(0, len(response), MAX_MESSAGE_LEN)]
        else:
            chunks = [response]

        for chunk in chunks:
            await post_to_matrix(chunk, room.room_id)

        log.info(f"Responded: {len(response)} chars")


async def invite_callback(room, event):
    """Auto-accept room invites for the bot."""
    if event.state_key and MATRIX_USER in event.state_key:
        log.info(f"Auto-accepting invite to {room.room_id}")
        await state.client.join(room.room_id)


# ── Main ──

async def main():
    required = {
        "MATRIX_USER": MATRIX_USER,
        "MATRIX_PASSWORD": MATRIX_PASSWORD,
        "MATRIX_ROOM_ID": ROOM_ID,
        "IRONCLAW_GATEWAY_URL": GATEWAY_URL,
        "IRONCLAW_GATEWAY_TOKEN": GATEWAY_TOKEN,
    }
    missing = [k for k, v in required.items() if not v]
    if missing:
        log.error(f"Missing config: {', '.join(missing)}")
        sys.exit(1)

    log.info("Matrix-IronClaw Bridge v3")
    log.info(f"  User: {MATRIX_USER_ID}")
    log.info(f"  Room: {ROOM_ID}")
    log.info(f"  Gateway: {BASE_URL}")

    await ensure_thread()

    sse_task = asyncio.create_task(sse_listener())
    await asyncio.sleep(2)

    state.client = AsyncClient(HOMESERVER, MATRIX_USER)
    resp = await state.client.login(MATRIX_PASSWORD)
    if not isinstance(resp, LoginResponse):
        log.error(f"Matrix login failed: {resp}")
        sys.exit(1)
    log.info(f"Logged in (device: {resp.device_id})")

    join_resp = await state.client.join(ROOM_ID)
    log.info(f"Room join: {type(join_resp).__name__}")

    log.info("Initial sync...")
    await state.client.sync(timeout=10000)
    state.startup_complete = True
    log.info("Listening for messages")

    state.client.add_event_callback(message_callback, RoomMessageText)
    state.client.add_event_callback(invite_callback, InviteMemberEvent)

    try:
        await state.client.sync_forever(timeout=30000)
    except KeyboardInterrupt:
        log.info("Shutting down")
    finally:
        sse_task.cancel()
        await state.client.close()


if __name__ == "__main__":
    asyncio.run(main())
