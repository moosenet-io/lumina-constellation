"""Standalone MemoryStore using local ./memory/ directory."""

import os, glob
from typing import List
from interfaces import MemoryStore


class LocalMemoryStore(MemoryStore):
    def __init__(self, memory_dir: str = './memory', conventions_file: str = './conventions.md'):
        self.memory_dir = memory_dir
        self.conventions_file = conventions_file
        os.makedirs(memory_dir, exist_ok=True)

    def query(self, topic: str, top_k: int = 3) -> List[str]:
        results = []
        keywords = topic.lower().split()
        for filepath in glob.glob(os.path.join(self.memory_dir, '*.md')):
            try:
                content = open(filepath).read()
                score = sum(1 for kw in keywords if kw in content.lower())
                if score > 0:
                    results.append((score, content[:500]))
            except Exception:
                pass
        results.sort(key=lambda x: -x[0])
        return [r[1] for r in results[:top_k]]

    def store(self, key: str, content: str) -> bool:
        safe_key = key.replace('/', '-').replace(' ', '_')[:50]
        filepath = os.path.join(self.memory_dir, f'{safe_key}.md')
        try:
            with open(filepath, 'a') as f:
                from datetime import datetime
                f.write(f'\n\n## {datetime.utcnow().strftime("%Y-%m-%d %H:%M")}\n{content}')
            return True
        except Exception:
            return False

    def get_conventions(self) -> str:
        try:
            return open(self.conventions_file).read()
        except Exception:
            return '# Conventions\nNo conventions file found. Create conventions.md.'
