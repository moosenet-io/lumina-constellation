#!/usr/bin/env python3
"""
Agent Loader — single source of truth for all Lumina agent definitions.
Reads .agent.yaml files from /opt/lumina-fleet/agents/
Falls back to constellation.yaml for backward compatibility.

Usage:
    from agent_loader import AgentLoader
    loader = AgentLoader()
    lumina = loader.get('lumina')
    print(lumina.display_name)  # "Lumina"
    all_agents = loader.list_agents()
"""

import os
import yaml
import glob
from pathlib import Path
from typing import Optional, Dict, Any
from dataclasses import dataclass, field

AGENTS_DIR = Path(os.environ.get('LUMINA_AGENTS_DIR', '/opt/lumina-fleet/agents'))
CONSTELLATION_YAML = Path('/opt/lumina-fleet/constellation.yaml')


@dataclass
class AgentRoute:
    type: str        # oauth, openrouter, litellm, ollama
    model: str
    enabled: bool = True
    endpoint: str = ''
    command: str = ''


@dataclass
class Agent:
    name: str
    display_name: str
    description: str = ''
    personality: str = ''
    system_prompt: str = ''
    routes: list = field(default_factory=list)
    tools: list = field(default_factory=list)
    refractor_categories: list = field(default_factory=list)
    engram_namespace: str = ''
    shared_namespaces: list = field(default_factory=list)
    channels: list = field(default_factory=list)
    container: str = ''
    runtime: str = 'ironclaw'
    auto_start: bool = False
    emoji: str = '🤖'
    source_file: str = ''

    @property
    def primary_model(self) -> str:
        """Return first enabled route's model."""
        for r in self.routes:
            if isinstance(r, dict) and r.get('enabled', True):
                return r.get('model', 'unknown')
        return 'unknown'

    @property
    def agent_id(self) -> str:
        return self.name


class AgentLoader:
    """Load and cache agent definitions from .agent.yaml files."""

    def __init__(self, agents_dir: str = None):
        self._dir = Path(agents_dir) if agents_dir else AGENTS_DIR
        self._cache: Dict[str, Agent] = {}
        self._loaded = False

    def _load_all(self):
        if self._loaded:
            return
        self._cache = {}

        # Load from .agent.yaml files
        if self._dir.exists():
            for path in sorted(self._dir.glob('*.agent.yaml')):
                try:
                    with open(path) as f:
                        data = yaml.safe_load(f)
                    if data and 'name' in data:
                        agent = self._from_dict(data, str(path))
                        self._cache[agent.name] = agent
                except Exception as e:
                    pass  # Skip malformed files

        # Fallback: load from constellation.yaml
        if CONSTELLATION_YAML.exists():
            try:
                with open(CONSTELLATION_YAML) as f:
                    cfg = yaml.safe_load(f) or {}
                for name, agent_cfg in cfg.get('agents', {}).items():
                    if name not in self._cache:
                        # Create minimal agent from constellation.yaml
                        agent = Agent(
                            name=name,
                            display_name=agent_cfg.get('display_name', name.title()),
                            description=agent_cfg.get('description', ''),
                            emoji=agent_cfg.get('emoji', '🤖'),
                            source_file=str(CONSTELLATION_YAML),
                        )
                        self._cache[name] = agent
            except Exception:
                pass

        self._loaded = True

    def _from_dict(self, data: dict, source_file: str = '') -> Agent:
        return Agent(
            name=data.get('name', ''),
            display_name=data.get('display_name', data.get('name', '').title()),
            description=data.get('description', ''),
            personality=data.get('personality', ''),
            system_prompt=data.get('system_prompt', ''),
            routes=data.get('routes', []),
            tools=data.get('tools', []),
            refractor_categories=data.get('refractor_categories', []),
            engram_namespace=data.get('engram', {}).get('namespace', f'agents/{data.get("name","unknown")}'),
            shared_namespaces=data.get('engram', {}).get('shared_namespaces', []),
            channels=data.get('channels', []),
            container=data.get('container', ''),
            runtime=data.get('runtime', 'ironclaw'),
            auto_start=data.get('auto_start', False),
            emoji=data.get('emoji', '🤖'),
            source_file=source_file,
        )

    def get(self, name: str) -> Optional[Agent]:
        """Get an agent by codename. Returns None if not found."""
        self._load_all()
        return self._cache.get(name)

    def get_display_name(self, name: str, fallback: str = None) -> str:
        """Get display name for an agent. Fallback to name.title() if not found."""
        agent = self.get(name)
        if agent:
            return agent.display_name
        return fallback or name.replace('-', ' ').replace('_', ' ').title()

    def list_agents(self) -> list:
        """Return list of all loaded agents."""
        self._load_all()
        return list(self._cache.values())

    def list_names(self) -> list:
        """Return list of all agent codenames."""
        self._load_all()
        return list(self._cache.keys())

    def to_dict(self) -> dict:
        """Return all agents as a dict (compatible with constellation.yaml format)."""
        self._load_all()
        return {name: {
            'display_name': a.display_name,
            'description': a.description,
            'emoji': a.emoji,
        } for name, a in self._cache.items()}

    def reload(self):
        """Force reload from disk."""
        self._loaded = False
        self._cache = {}


# Module-level singleton for easy import
_default_loader = None


def _get_loader() -> AgentLoader:
    global _default_loader
    if _default_loader is None:
        _default_loader = AgentLoader()
    return _default_loader


def display_name(agent_name: str, fallback: str = None) -> str:
    """Convenience function — get display name for an agent codename."""
    return _get_loader().get_display_name(agent_name, fallback)


def get_agent(name: str) -> Optional[Agent]:
    """Convenience function — get an Agent object by codename."""
    return _get_loader().get(name)


def load_agents(agents_dir: str = None) -> Dict[str, Agent]:
    """Convenience function — load all agents, return {name: Agent} dict."""
    loader = AgentLoader(agents_dir) if agents_dir else _get_loader()
    loader._load_all()
    return loader._cache


if __name__ == '__main__':
    loader = AgentLoader()
    agents = loader.list_agents()
    print(f"Loaded {len(agents)} agents from {loader._dir}")
    for a in agents:
        print(f"  [{a.name}] {a.display_name} — {a.description[:50]}")
        if a.primary_model != 'unknown':
            print(f"    model: {a.primary_model}, container: {a.container or 'N/A'}")
