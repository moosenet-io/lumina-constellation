# Vector Conventions — MooseNet

## Shell / Python
- Always use python3, not python (CT310 only has python3 in PATH)
- Test commands: python3 -m pytest, not pytest directly
- Scripts must use #!/usr/bin/env python3 shebang

## Code style
- snake_case for Python identifiers
- Imperative commit messages: Add, Fix, Update, not Added
- One logical change per commit

## Git
- Always push to a branch, never main directly
- PR title: descriptive, under 70 chars

## Environment
- CT310 runs Debian 12, Python 3.11
- Packages: psycopg2-binary, requests, pyyaml, sqlite-vec installed
- LiteLLM at http://<litellm-ip>:4000
