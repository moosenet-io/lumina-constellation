"""
spectra_sanitizer.py — 10-stage content sanitization pipeline. (BA.3)
Runs on Terminus. Called by spectra_tools.py before any content
reaches LLM inference or Engram storage.

Security: strips prompt injection, hidden elements, scripts, data URIs,
zero-width chars. Wraps output in [UNTRUSTED_WEB_CONTENT] delimiters.
"""

import re
import unicodedata
from bs4 import BeautifulSoup, Comment

MAX_OUTPUT_TOKENS = 2000
APPROX_CHARS_PER_TOKEN = 4  # conservative estimate

ZERO_WIDTH_CHARS = [
    '\u200b',  # zero-width space
    '\u200c',  # zero-width non-joiner
    '\u200d',  # zero-width joiner
    '\u2060',  # word joiner
    '\ufeff',  # zero-width no-break space (BOM)
    '\u00ad',  # soft hyphen
]

DELIMITER_START = "[UNTRUSTED_WEB_CONTENT]\n"
DELIMITER_END   = "\n[/UNTRUSTED_WEB_CONTENT]"


def sanitize(html: str) -> tuple[str, list]:
    """
    Run 10-stage sanitization pipeline.
    Returns (sanitized_text, flags) where flags is a list of detected issues.
    """
    flags = []

    # ── Stage 1: Parse with BeautifulSoup ────────────────────────────────────
    soup = BeautifulSoup(html, "html.parser")

    # ── Stage 2: Remove dangerous tags entirely ───────────────────────────────
    dangerous_tags = ["script", "style", "noscript", "iframe", "object",
                      "embed", "applet", "form", "input", "textarea", "select"]
    for tag in dangerous_tags:
        for el in soup.find_all(tag):
            el.decompose()

    # ── Stage 3: Remove hidden elements ──────────────────────────────────────
    hidden_selectors = []
    for el in soup.find_all(style=True):
        style = el.get("style", "")
        if any(x in style.lower() for x in ["display:none", "display: none",
                                              "visibility:hidden", "visibility: hidden",
                                              "opacity:0", "opacity: 0"]):
            flags.append("hidden_element_stripped")
            el.decompose()
    for el in soup.find_all(hidden=True):
        el.decompose()
    for el in soup.find_all(attrs={"aria-hidden": "true"}):
        el.decompose()

    # ── Stage 4: Remove HTML comments ─────────────────────────────────────────
    for comment in soup.find_all(string=lambda t: isinstance(t, Comment)):
        comment.extract()
        flags.append("html_comment_stripped")

    # ── Stage 5: Remove zero-width characters from all text ──────────────────
    for el in soup.find_all(string=True):
        cleaned = el
        for zwc in ZERO_WIDTH_CHARS:
            cleaned = cleaned.replace(zwc, "")
        if cleaned != el:
            el.replace_with(cleaned)
            flags.append("zero_width_chars_removed")

    # ── Stage 6: Remove data: URIs ────────────────────────────────────────────
    for el in soup.find_all(src=re.compile(r'^data:', re.I)):
        el.decompose()
        flags.append("data_uri_removed")
    for el in soup.find_all(href=re.compile(r'^data:', re.I)):
        el.decompose()
        flags.append("data_uri_removed")

    # ── Stage 7: Extract visible text ─────────────────────────────────────────
    text = soup.get_text(separator=" ", strip=True)

    # ── Stage 8: Normalize whitespace ─────────────────────────────────────────
    text = re.sub(r'[ \t]+', ' ', text)           # collapse spaces/tabs
    text = re.sub(r'\n{3,}', '\n\n', text)         # max 2 consecutive newlines
    text = re.sub(r' *\n *', '\n', text)            # trim spaces around newlines
    text = text.strip()

    # ── Stage 9: Truncate to token budget ────────────────────────────────────
    max_chars = MAX_OUTPUT_TOKENS * APPROX_CHARS_PER_TOKEN
    if len(text) > max_chars:
        text = text[:max_chars]
        # Trim to last word boundary
        last_space = text.rfind(' ')
        if last_space > max_chars * 0.9:
            text = text[:last_space]
        text += "\n[CONTENT TRUNCATED — token budget exceeded]"
        flags.append("truncated")

    # ── Stage 10: Wrap in delimiters ─────────────────────────────────────────
    text = DELIMITER_START + text + DELIMITER_END

    return text, list(set(flags))


def sanitize_double(html: str, consumer_key: str = "") -> tuple[str, list]:
    """
    Double-extraction for high-trust consumers (Vigil, Seer).
    Runs sanitize() twice and checks consistency.
    Falls back to empty string on disagreement.
    """
    text1, flags1 = sanitize(html)
    text2, flags2 = sanitize(html)

    # Simple consistency check: both passes should produce identical output
    if text1 != text2:
        return DELIMITER_START + "[EXTRACTION DISAGREEMENT — content dropped for safety]" + DELIMITER_END, ["extraction_disagreement"]

    return text1, flags1


def strip_for_prompt(text: str) -> str:
    """
    Additional prompt-injection stripping for when content goes directly
    into an LLM system prompt. Removes common injection patterns.
    """
    injection_patterns = [
        r'ignore\s+(previous|all)\s+instructions',
        r'disregard\s+(previous|all)\s+instructions',
        r'forget\s+(everything|all)',
        r'you\s+are\s+now\s+',
        r'new\s+system\s+prompt',
        r'act\s+as\s+',
        r'roleplay\s+as\s+',
        r'jailbreak',
        r'DAN\s+mode',
    ]
    cleaned = text
    for pattern in injection_patterns:
        cleaned = re.sub(pattern, '[FILTERED]', cleaned, flags=re.IGNORECASE)
    return cleaned
