"""
Vector interface contracts — abstract base classes for all backends.
The core loop only imports from this module, never from backend implementations.
This enables swapping backends (standalone ↔ integrated) without touching core code.
"""

from abc import ABC, abstractmethod
from typing import Optional, List, Dict, Any
from dataclasses import dataclass, field
from datetime import datetime
import uuid


@dataclass
class Task:
    id: str = field(default_factory=lambda: str(uuid.uuid4()))
    name: str = ''
    description: str = ''
    status: str = 'queued'  # queued | running | done | failed | escalated
    result: Optional[Dict] = None
    created_at: str = field(default_factory=lambda: datetime.utcnow().isoformat() + 'Z')
    updated_at: str = field(default_factory=lambda: datetime.utcnow().isoformat() + 'Z')
    metadata: Dict = field(default_factory=dict)


@dataclass
class Message:
    id: str = field(default_factory=lambda: str(uuid.uuid4()))
    from_agent: str = 'vector'
    to_agent: str = 'lumina'
    message_type: str = 'status'
    payload: Dict = field(default_factory=dict)
    priority: str = 'normal'
    correlation_id: str = ''


class StateBackend(ABC):
    """Manages task state: creation, updates, querying, completion.

    Standalone: SQLite database.
    Integrated: Plane CE API (The Plexus project).
    """

    @abstractmethod
    def create_task(self, task: Task) -> Task:
        """Create a new task. Returns task with backend-assigned ID if applicable."""
        raise NotImplementedError

    @abstractmethod
    def update_status(self, task_id: str, status: str, result: Optional[Dict] = None) -> bool:
        """Update task status. Returns True on success."""
        raise NotImplementedError

    @abstractmethod
    def get_task(self, task_id: str) -> Optional[Task]:
        """Get task by ID. Returns None if not found."""
        raise NotImplementedError

    @abstractmethod
    def list_tasks(self, status_filter: Optional[str] = None) -> List[Task]:
        """List tasks, optionally filtered by status."""
        raise NotImplementedError

    @abstractmethod
    def complete_task(self, task_id: str, result: Dict) -> bool:
        """Mark task complete with result. Returns True on success."""
        raise NotImplementedError


class MessageBus(ABC):
    """Sends and receives messages between Vector and other agents.

    Standalone: stdout + log file.
    Integrated: Nexus inbox (direct psycopg2 to postgres-host).
    """

    @abstractmethod
    def send(self, to: str, msg_type: str, payload: Dict, priority: str = 'normal', correlation_id: str = '') -> Optional[str]:
        """Send a message. Returns message_id or None on failure."""
        raise NotImplementedError

    @abstractmethod
    def check(self) -> Dict:
        """Check for incoming messages. Returns {pending: int, by_priority: {}}."""
        raise NotImplementedError

    @abstractmethod
    def read(self, limit: int = 5) -> List[Dict]:
        """Read pending messages. Marks them as read."""
        raise NotImplementedError

    @abstractmethod
    def ack(self, message_ids: List[str]) -> bool:
        """Acknowledge (mark processed) a list of message IDs."""
        raise NotImplementedError


class MemoryStore(ABC):
    """Queries and stores institutional memory across Vector runs.

    Standalone: local ./memory/ directory (grep-based search).
    Integrated: Engram (sqlite-vec embeddings + Gitea).
    """

    @abstractmethod
    def query(self, topic: str, top_k: int = 3) -> List[str]:
        """Search memory for relevant context about a topic. Returns list of relevant excerpts."""
        raise NotImplementedError

    @abstractmethod
    def store(self, key: str, content: str) -> bool:
        """Save a learning or decision. Returns True on success."""
        raise NotImplementedError

    @abstractmethod
    def get_conventions(self) -> str:
        """Load project conventions (naming, architecture, git rules). Returns markdown text."""
        raise NotImplementedError


class CostGate(ABC):
    """Controls inference spend. Prevents runaway costs.

    Standalone: hard cap per run (default $5.00).
    Integrated: Nexus approval flow with Lumina.
    """

    @abstractmethod
    def check_budget(self, estimated_cost: float) -> bool:
        """Can we afford this operation? Returns True if within budget."""
        raise NotImplementedError

    @abstractmethod
    def record_spend(self, amount: float, description: str = '') -> None:
        """Log actual spend."""
        raise NotImplementedError

    @abstractmethod
    def get_remaining(self) -> float:
        """How much budget remains for this run?"""
        raise NotImplementedError

    @abstractmethod
    def request_approval(self, reason: str, amount_needed: float) -> bool:
        """Request approval for additional budget. Returns True if approved."""
        raise NotImplementedError
