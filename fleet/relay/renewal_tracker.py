#!/usr/bin/env python3
"""
Lumina Fleet — Relay Document Renewal Tracker
RLY-9: Tracks document expiry dates and surfaces upcoming renewals.

Storage: SQLite at /opt/lumina-fleet/relay/renewals.db
Usage:
    python3 renewal_tracker.py list [--days 90]
    python3 renewal_tracker.py overdue
    python3 renewal_tracker.py add --type "car insurance" --expiry 2026-09-15 --notes "State Farm"
"""

import argparse
import json
import sqlite3
import sys
from datetime import date, datetime, timedelta
from pathlib import Path

DB_PATH = Path("/opt/lumina-fleet/relay/renewals.db")


# ============================================================
# DB init
# ============================================================

def _get_conn() -> sqlite3.Connection:
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    conn.execute("""
        CREATE TABLE IF NOT EXISTS document_renewals (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            doc_type    TEXT    NOT NULL,
            expiry_date TEXT    NOT NULL,
            owner       TEXT    NOT NULL DEFAULT 'peter',
            notes       TEXT    NOT NULL DEFAULT '',
            created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT    NOT NULL DEFAULT (datetime('now'))
        )
    """)
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_expiry ON document_renewals(expiry_date)"
    )
    conn.commit()
    return conn


# ============================================================
# Core functions
# ============================================================

def store_renewal(doc_type: str, expiry_date: str, owner: str = "peter", notes: str = "") -> dict:
    """
    Store or update a document renewal record.
    expiry_date must be in YYYY-MM-DD format.
    Returns the saved record as a dict.
    """
    # Validate date format
    try:
        datetime.strptime(expiry_date, "%Y-%m-%d")
    except ValueError:
        return {"error": f"Invalid date format '{expiry_date}'. Use YYYY-MM-DD."}

    conn = _get_conn()
    now = datetime.utcnow().isoformat(timespec="seconds")

    # Upsert: if same doc_type + owner already exists, update it
    existing = conn.execute(
        "SELECT id FROM document_renewals WHERE doc_type = ? AND owner = ?",
        (doc_type, owner),
    ).fetchone()

    if existing:
        conn.execute(
            """UPDATE document_renewals
               SET expiry_date=?, notes=?, updated_at=?
               WHERE id=?""",
            (expiry_date, notes, now, existing["id"]),
        )
        row_id = existing["id"]
        action = "updated"
    else:
        cur = conn.execute(
            """INSERT INTO document_renewals (doc_type, expiry_date, owner, notes, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?)""",
            (doc_type, expiry_date, owner, notes, now, now),
        )
        row_id = cur.lastrowid
        action = "created"

    conn.commit()
    row = conn.execute(
        "SELECT * FROM document_renewals WHERE id = ?", (row_id,)
    ).fetchone()
    conn.close()
    return {action: True, "record": _row_to_dict(row)}


def get_upcoming_renewals(days_ahead: int = 90) -> list:
    """
    Return documents expiring within the next N days (inclusive today).
    Sorted by expiry date ascending.
    """
    today = date.today()
    cutoff = today + timedelta(days=days_ahead)
    conn = _get_conn()
    rows = conn.execute(
        """SELECT * FROM document_renewals
           WHERE expiry_date >= ? AND expiry_date <= ?
           ORDER BY expiry_date ASC""",
        (today.isoformat(), cutoff.isoformat()),
    ).fetchall()
    conn.close()
    results = [_row_to_dict(r) for r in rows]
    for r in results:
        r["days_until_expiry"] = (
            datetime.strptime(r["expiry_date"], "%Y-%m-%d").date() - today
        ).days
    return results


def get_overdue_renewals() -> list:
    """Return documents already expired (expiry_date < today). Sorted oldest first."""
    today = date.today()
    conn = _get_conn()
    rows = conn.execute(
        """SELECT * FROM document_renewals
           WHERE expiry_date < ?
           ORDER BY expiry_date ASC""",
        (today.isoformat(),),
    ).fetchall()
    conn.close()
    results = [_row_to_dict(r) for r in rows]
    for r in results:
        r["days_overdue"] = (
            today - datetime.strptime(r["expiry_date"], "%Y-%m-%d").date()
        ).days
    return results


def delete_renewal(doc_type: str, owner: str = "peter") -> dict:
    """Delete a renewal record by doc_type + owner."""
    conn = _get_conn()
    cur = conn.execute(
        "DELETE FROM document_renewals WHERE doc_type = ? AND owner = ?",
        (doc_type, owner),
    )
    conn.commit()
    conn.close()
    if cur.rowcount:
        return {"deleted": True, "doc_type": doc_type, "owner": owner}
    return {"deleted": False, "reason": "record not found"}


# ============================================================
# Internal helpers
# ============================================================

def _row_to_dict(row: sqlite3.Row) -> dict:
    return dict(row)


# ============================================================
# CLI
# ============================================================

def _print_json(obj):
    print(json.dumps(obj, indent=2, default=str))


def main():
    parser = argparse.ArgumentParser(
        description="Lumina Fleet — Document Renewal Tracker (RLY-9)"
    )
    sub = parser.add_subparsers(dest="command", required=True)

    # list
    p_list = sub.add_parser("list", help="List upcoming renewals")
    p_list.add_argument(
        "--days", type=int, default=90,
        help="How many days ahead to look (default: 90)"
    )

    # overdue
    sub.add_parser("overdue", help="List overdue (already expired) documents")

    # add
    p_add = sub.add_parser("add", help="Add or update a renewal record")
    p_add.add_argument("--type", dest="doc_type", required=True, help="Document type")
    p_add.add_argument("--expiry", required=True, help="Expiry date (YYYY-MM-DD)")
    p_add.add_argument("--owner", default="peter", help="Owner (default: peter)")
    p_add.add_argument("--notes", default="", help="Optional notes")

    # delete
    p_del = sub.add_parser("delete", help="Delete a renewal record")
    p_del.add_argument("--type", dest="doc_type", required=True, help="Document type")
    p_del.add_argument("--owner", default="peter")

    args = parser.parse_args()

    if args.command == "list":
        results = get_upcoming_renewals(days_ahead=args.days)
        _print_json({
            "command": "list",
            "days_ahead": args.days,
            "count": len(results),
            "renewals": results,
        })

    elif args.command == "overdue":
        results = get_overdue_renewals()
        _print_json({
            "command": "overdue",
            "count": len(results),
            "overdue": results,
        })

    elif args.command == "add":
        result = store_renewal(
            doc_type=args.doc_type,
            expiry_date=args.expiry,
            owner=args.owner,
            notes=args.notes,
        )
        _print_json(result)
        if "error" in result:
            sys.exit(1)

    elif args.command == "delete":
        result = delete_renewal(doc_type=args.doc_type, owner=args.owner)
        _print_json(result)


if __name__ == "__main__":
    main()
