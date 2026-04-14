"""Integrated StateBackend using Plane CE (The Plexus project)."""
import os, json, urllib.request, time
from datetime import datetime
from backends.interfaces import StateBackend, Task

PLANE_BASE = os.environ.get('PLANE_BASE_URL', 'http://YOUR_PLANE_IP')
PLANE_TOKEN = os.environ.get('PLANE_TOKEN_LUMINA', '')
PLANE_WS = os.environ.get('PLANE_WORKSPACE_SLUG', 'moosenet')
PX_PROJECT_ID = os.environ.get('PX_PROJECT_ID', '507ff56c-772d-462e-a79c-2d93783968ff')

class PlaneState(StateBackend):
    def __init__(self, config=None):
        self.base = PLANE_BASE
        self.token = PLANE_TOKEN
        self.ws = PLANE_WS
        self.project_id = PX_PROJECT_ID
        self._states = {}  # cached: name → id

    def _plane(self, method, path, data=None):
        req = urllib.request.Request(
            f'{self.base}{path}',
            data=json.dumps(data).encode() if data else None,
            headers={'X-API-Key': self.token, 'Content-Type': 'application/json'},
            method=method)
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.load(r)

    def _get_state_id(self, state_name):
        if not self._states:
            d = self._plane('GET', f'/api/v1/workspaces/{self.ws}/projects/{self.project_id}/states/')
            self._states = {s['name']: s['id'] for s in d.get('results', [])}
        return self._states.get(state_name, self._states.get('Queued', list(self._states.values())[0] if self._states else ''))

    def create_task(self, task: Task) -> Task:
        data = {'name': task.name[:100], 'description_html': f'<p>{task.description}</p>',
                'state': self._get_state_id('Queued'), 'priority': 'medium'}
        d = self._plane('POST', f'/api/v1/workspaces/{self.ws}/projects/{self.project_id}/issues/', data)
        task.metadata['plane_id'] = d.get('id', '')
        task.metadata['sequence_id'] = d.get('sequence_id', '')
        return task

    def update_status(self, task_id, status, result=None):
        plane_id = task_id if len(task_id) > 20 else None
        # task_id might be the plane UUID or our own ID
        # Try to find by metadata
        state_map = {'queued': 'Queued', 'running': 'Running', 'done': 'Done', 'failed': 'Failed', 'escalated': 'Escalated'}
        state_id = self._get_state_id(state_map.get(status.lower(), 'Todo'))
        if plane_id:
            try:
                self._plane('PATCH', f'/api/v1/workspaces/{self.ws}/projects/{self.project_id}/issues/{plane_id}/', {'state': state_id})
            except Exception:
                pass
        return True

    def get_task(self, task_id):
        try:
            d = self._plane('GET', f'/api/v1/workspaces/{self.ws}/projects/{self.project_id}/issues/{task_id}/')
            state_name = self._states.get(d.get('state', ''), 'unknown')
            return Task(id=d['id'], name=d.get('name',''), status=state_name.lower())
        except Exception:
            return None

    def list_tasks(self, status_filter=None):
        d = self._plane('GET', f'/api/v1/workspaces/{self.ws}/projects/{self.project_id}/issues/?per_page=50')
        tasks = []
        state_map = {v: k for k, v in self._states.items()}
        for i in d.get('results', []):
            state_name = state_map.get(i.get('state',''), 'unknown').lower()
            if status_filter is None or state_name == status_filter:
                tasks.append(Task(id=i['id'], name=i.get('name',''), status=state_name))
        return tasks

    def complete_task(self, task_id, result):
        return self.update_status(task_id, 'done', result)
