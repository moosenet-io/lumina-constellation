# Security Policy

Lumina Constellation is a personal AI assistant that handles private data,
credentials, and conversations. Security and privacy are core design goals, not
afterthoughts.

## Reporting a vulnerability

If you discover a security vulnerability, please report it responsibly:

- **Do not** open a public issue for security-sensitive reports.
- Use GitHub's private vulnerability reporting ("Report a vulnerability" under
  the repository's **Security** tab) to disclose the issue privately.
- Include a clear description, reproduction steps, affected component, and the
  potential impact.

We aim to acknowledge reports promptly and will keep you informed as we
investigate and address the issue. Please give us a reasonable opportunity to
release a fix before any public disclosure.

## Supported versions

Security fixes are applied to the `main` branch. Until a formal release cadence
is established, `main` is the supported version.

## Security design principles

Lumina is built around a defense-in-depth posture:

- **Vault-based secrets.** Credentials are stored in an encrypted vault and
  resolved at runtime. Secrets are never hardcoded in source, tests, or
  configuration committed to the repository.
- **No hardcoded credentials.** All configuration — API keys, tokens, hosts —
  is supplied through environment variables. Example/placeholder values in the
  codebase are intentionally non-functional.
- **PII gate.** A pre-commit / pre-push gate scans for secrets, private IP
  ranges, internal hostnames, and other sensitive identifiers. The public
  repository runs the strictest posture and blocks bypass attempts.
- **Egress allowlisting.** Outbound network access from tools is restricted to
  an explicit allowlist; private/loopback destinations are blocked unless
  explicitly permitted by the operator.
- **Input & output guards.** Prompt-injection scanning on input and PII
  redaction on output protect the model and downstream consumers.
- **Least-privilege tooling.** Sensitive operations (e.g. infrastructure
  automation) are gated behind explicit operator approval.

## Handling secrets when contributing

- Never commit real secrets, private IP addresses, internal hostnames, personal
  data, or infrastructure details.
- Use placeholders (`<YOUR_TOKEN>`, `example.com`, RFC 5737 documentation IPs
  such as `192.0.2.x`) in code, tests, and docs.
- If you believe a secret has been committed, treat it as compromised: rotate it
  immediately and report it through the channel above.
