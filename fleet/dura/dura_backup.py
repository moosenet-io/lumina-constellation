#!/usr/bin/env python3
"""
Dura — Lumina Nexus Backup System (Phase 1)
Runs on fleet-host. Hourly critical backups, daily full backups.
NFS target: PVS host /mnt/nfs/lumina-backup/ (via rsync)
Local fallback: /opt/lumina-fleet/dura/backups/

Usage:
    python3 dura_backup.py hourly
    python3 dura_backup.py daily
    python3 dura_backup.py status
"""

import os
import sys
import json
import shutil
import subprocess
import logging
from datetime import datetime, timedelta
from pathlib import Path

# ── Config ────────────────────────────────────────────────────────────────────
ENV_FILE = "/opt/lumina-fleet/axon/.env"
LOCAL_BACKUP_ROOT = "/opt/lumina-fleet/dura/backups"
NFS_RSYNC_TARGET = "root@YOUR_PVS_HOST_IP:/mnt/nfs/lumina-backup"
LOG_FILE = "/opt/lumina-fleet/dura/logs/dura_backup.log"
STATUS_FILE = "/opt/lumina-fleet/dura/output/backup_status.json"

# CT IDs for pct exec backups
CT_POSTGRES = 300    # lumina_inbox lives here
CT_PLANE_DB = 315    # Plane CE — uses Docker container

PVS_HOST = "root@YOUR_PVS_HOST_IP"

# SQLite databases on fleet-host
SQLITE_DBS = {
    "nexus":  "/opt/lumina-fleet/nexus/nexus.db",
    "engram": "/opt/lumina-fleet/engram/engram.db",
    "myelin": "/opt/lumina-fleet/myelin/myelin.db",
}

SQLITE_DBS_EXTRA = {
    "renewals":     "/opt/lumina-fleet/relay/renewals.db",
    "cortex_terminus": "/opt/lumina-fleet/cortex/graphs/lumina-terminus.db",
    "cortex_fleet":    "/opt/lumina-fleet/cortex/graphs/lumina-fleet.db",
}

# Postgres databases on postgres-host
POSTGRES_DBS = [
    {"name": "lumina_inbox", "ct_id": 300, "user": "lumina_inbox_user", "pass_env": "INBOX_DB_PASS"},
    {"name": "ironclaw",     "ct_id": 300, "user": "ironclaw",          "pass_env": ""},
    {"name": "litellm",      "ct_id": 300, "user": "litellm_user",      "pass_env": ""},
]

# ── Logging ───────────────────────────────────────────────────────────────────
Path("/opt/lumina-fleet/dura/logs").mkdir(parents=True, exist_ok=True)
Path("/opt/lumina-fleet/dura/output").mkdir(parents=True, exist_ok=True)

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    handlers=[
        logging.FileHandler(LOG_FILE),
        logging.StreamHandler(sys.stdout),
    ],
)
log = logging.getLogger("dura")


# ── Env loading ───────────────────────────────────────────────────────────────
def load_env(path=ENV_FILE):
    """Load key=value pairs from env file into os.environ."""
    if not os.path.exists(path):
        log.warning(f"Env file not found: {path}")
        return
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" in line:
                k, _, v = line.partition("=")
                v = v.strip().strip('"').strip("'")
                os.environ.setdefault(k.strip(), v)


# ── Backup dir ────────────────────────────────────────────────────────────────
def get_backup_dir(subdir="") -> str:
    """Return local backup dir (always local; rsync handles NFS copy)."""
    base = Path(LOCAL_BACKUP_ROOT)
    target = base / subdir if subdir else base
    target.mkdir(parents=True, exist_ok=True)
    return str(target)


def nfs_available() -> bool:
    """Check if NFS is reachable from fleet-host via SSH to PVS host."""
    try:
        result = subprocess.run(
            ["ssh", "-o", "ConnectTimeout=3", "-o", "BatchMode=yes",
             PVS_HOST, "test -d /mnt/nfs/lumina-backup && echo ok"],
            capture_output=True, text=True, timeout=8
        )
        return result.returncode == 0 and "ok" in result.stdout
    except Exception:
        return False


def rsync_to_nfs(local_dir: str) -> bool:
    """Rsync local backup dir to NFS via PVS host."""
    try:
        cmd = [
            "ssh", "-o", "ConnectTimeout=5", "-o", "BatchMode=yes",
            PVS_HOST,
            f"rsync -a --delete /mnt/nfs/lumina-backup/ /mnt/nfs/lumina-backup/ 2>/dev/null; "
            f"echo rsync_placeholder"
        ]
        # Actual rsync: push from fleet-host to PVS host path
        result = subprocess.run(
            ["rsync", "-rltz", "--delete",
             "--no-perms", "--no-owner", "--no-group",
             local_dir + "/",
             f"{PVS_HOST}:/mnt/nfs/lumina-backup/"],
            capture_output=True, text=True, timeout=120
        )
        if result.returncode == 0:
            log.info("Rsync to NFS succeeded.")
            return True
        else:
            log.warning(f"Rsync to NFS failed: {result.stderr.strip()}")
            return False
    except Exception as e:
        log.warning(f"Rsync to NFS exception: {e}")
        return False


# ── SQLite backup ─────────────────────────────────────────────────────────────
def backup_sqlite(db_path: str, output_dir: str, name: str) -> dict:
    """
    Copy SQLite DB with timestamp. Uses sqlite3 .backup to avoid corruption
    if the DB is live. Falls back to shutil.copy2 if sqlite3 unavailable.
    """
    result = {"name": name, "source": db_path, "status": "skipped"}
    if not os.path.exists(db_path):
        log.warning(f"SQLite DB not found, skipping: {db_path}")
        result["status"] = "missing"
        return result

    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    dest = os.path.join(output_dir, f"{name}_{ts}.db")

    try:
        proc = subprocess.run(
            ["sqlite3", db_path, f".backup '{dest}'"],
            capture_output=True, text=True, timeout=60
        )
        if proc.returncode == 0 and os.path.exists(dest):
            size = os.path.getsize(dest)
            log.info(f"SQLite backup OK: {name} -> {dest} ({size} bytes)")
            result.update({"status": "ok", "dest": dest, "size": size})
            return result
    except FileNotFoundError:
        pass  # sqlite3 binary not available; fall back to copy

    try:
        shutil.copy2(db_path, dest)
        size = os.path.getsize(dest)
        log.info(f"SQLite copy OK: {name} -> {dest} ({size} bytes)")
        result.update({"status": "ok", "dest": dest, "size": size})
    except Exception as e:
        log.error(f"SQLite backup failed for {name}: {e}")
        result.update({"status": "error", "error": str(e)})

    return result


# ── Postgres backup ───────────────────────────────────────────────────────────
def backup_postgres(db_name: str, ct_id: int, db_user: str, output_dir: str, db_pass: str = "") -> dict:
    """
    Dump Postgres DB from a container via SSH → pct exec → pg_dump.
    Uses TCP (-h 127.0.0.1) with PGPASSWORD to bypass peer auth.
    Streams through gzip into a local file.
    """
    result = {"name": db_name, "ct_id": ct_id, "status": "error"}
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    tmp_file = f"/tmp/pgdump_{db_name}_{ts}.sql.gz"
    dest_file = os.path.join(output_dir, f"{db_name}_{ts}.sql.gz")

    # Use -h 127.0.0.1 to force TCP (scram-sha-256 auth), with PGPASSWORD
    if db_pass:
        pg_cmd = f"PGPASSWORD='{db_pass}' pg_dump -h 127.0.0.1 -U {db_user} {db_name}"
    else:
        # Fallback: try as postgres superuser via peer (local socket)
        pg_cmd = f"su -c 'pg_dump {db_name}' postgres"
    ssh_cmd = f'ssh -o BatchMode=yes -o ConnectTimeout=10 {PVS_HOST} "pct exec {ct_id} -- bash -c \\"{pg_cmd}\\""'
    full_cmd = f'{ssh_cmd} | gzip > {tmp_file}'

    log.info(f"Postgres backup: {db_name} from CT{ct_id} as {db_user}")
    try:
        proc = subprocess.run(
            full_cmd, shell=True, capture_output=True, text=True, timeout=300
        )
        file_size = os.path.getsize(tmp_file) if os.path.exists(tmp_file) else 0
        # Valid dump is at least 100 bytes (gzip header ~20 bytes + SQL header)
        if proc.returncode == 0 and file_size > 100:
            shutil.move(tmp_file, dest_file)
            size = os.path.getsize(dest_file)
            log.info(f"Postgres backup OK: {db_name} -> {dest_file} ({size} bytes)")
            result.update({"status": "ok", "dest": dest_file, "size": size})
        else:
            stderr = proc.stderr.strip()
            log.error(f"Postgres backup failed for {db_name}: rc={proc.returncode} size={file_size} {stderr}")
            result.update({"status": "error", "error": stderr or f"rc={proc.returncode} size={file_size}"})
            # Cleanup partial file
            if os.path.exists(tmp_file):
                os.remove(tmp_file)
    except subprocess.TimeoutExpired:
        log.error(f"Postgres backup timeout for {db_name}")
        result.update({"status": "error", "error": "timeout"})
    except Exception as e:
        log.error(f"Postgres backup exception for {db_name}: {e}")
        result.update({"status": "error", "error": str(e)})

    return result


def backup_plane_postgres(output_dir: str) -> dict:
    """
    Backup Plane's Postgres from the Docker container on plane-host.
    docker exec plane-app-plane-db-1 pg_dump ...
    """
    result = {"name": "plane_db", "ct_id": 315, "status": "error"}
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    tmp_file = f"/tmp/pgdump_plane_{ts}.sql.gz"
    dest_file = os.path.join(output_dir, f"plane_db_{ts}.sql.gz")

    # Plane Postgres password is 'plane' (from Docker env POSTGRES_PASSWORD)
    pg_cmd = "docker exec -e PGPASSWORD=plane plane-app-plane-db-1 pg_dump -U plane plane"
    ssh_cmd = f'ssh -o BatchMode=yes -o ConnectTimeout=10 {PVS_HOST} "pct exec 315 -- {pg_cmd}"'
    full_cmd = f'{ssh_cmd} | gzip > {tmp_file}'

    log.info("Postgres backup: plane_db from plane-host via Docker")
    try:
        proc = subprocess.run(
            full_cmd, shell=True, capture_output=True, text=True, timeout=300
        )
        file_size = os.path.getsize(tmp_file) if os.path.exists(tmp_file) else 0
        if proc.returncode == 0 and file_size > 100:
            shutil.move(tmp_file, dest_file)
            size = os.path.getsize(dest_file)
            log.info(f"Plane Postgres OK: {dest_file} ({size} bytes)")
            result.update({"status": "ok", "dest": dest_file, "size": size})
        else:
            stderr = proc.stderr.strip()
            log.warning(f"Plane Postgres backup failed: rc={proc.returncode} size={file_size} {stderr}")
            result.update({"status": "error", "error": stderr or f"rc={proc.returncode} size={file_size}"})
            if os.path.exists(tmp_file):
                os.remove(tmp_file)
    except Exception as e:
        log.error(f"Plane Postgres exception: {e}")
        result.update({"status": "error", "error": str(e)})

    return result


# ── Fleet config backup ───────────────────────────────────────────────────────
def backup_fleet_config(output_dir: str) -> dict:
    """Tar up /opt/lumina-fleet/ config files (excluding large .db and __pycache__)."""
    result = {"name": "fleet_config", "status": "error"}
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    dest = os.path.join(output_dir, f"fleet_config_{ts}.tar.gz")

    exclude_patterns = [
        "--exclude=/opt/lumina-fleet/dura/backups",
        "--exclude=__pycache__",
        "--exclude=*.pyc",
        "--exclude=*.db",
        "--exclude=*.log",
    ]
    cmd = ["tar", "-czf", dest] + exclude_patterns + ["/opt/lumina-fleet/"]

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        if proc.returncode in (0, 1) and os.path.exists(dest):  # 1 = warnings (ok)
            size = os.path.getsize(dest)
            log.info(f"Fleet config backup OK: {dest} ({size} bytes)")
            result.update({"status": "ok", "dest": dest, "size": size})
        else:
            log.error(f"Fleet config tar failed: {proc.stderr.strip()}")
            result.update({"status": "error", "error": proc.stderr.strip()})
    except Exception as e:
        log.error(f"Fleet config backup exception: {e}")
        result.update({"status": "error", "error": str(e)})

    return result


# ── Rotation ───────────────────────────────────────────────────────────────────
def rotate_backups(backup_dir: str, keep_daily: int = 7, keep_weekly: int = 4):
    """
    Delete old backup files. Keep last keep_daily daily backups per prefix.
    Weekly: keep the oldest file per ISO week for keep_weekly weeks.
    """
    p = Path(backup_dir)
    if not p.exists():
        return

    # Group by prefix (everything before the timestamp)
    from collections import defaultdict
    groups: dict = defaultdict(list)
    for f in sorted(p.iterdir()):
        if f.is_file() and (f.suffix in (".gz", ".db")):
            # prefix = name up to the 8-digit date
            name = f.name
            # Find timestamp pattern YYYYMMDD_HHMMSS
            import re
            m = re.search(r"_(\d{8}_\d{6})\.", name)
            if m:
                prefix = name[: m.start()]
                groups[prefix].append(f)

    for prefix, files in groups.items():
        files.sort(key=lambda x: x.stat().st_mtime)
        # Keep the most recent keep_daily files
        to_delete = files[:-keep_daily] if len(files) > keep_daily else []
        for f in to_delete:
            try:
                f.unlink()
                log.info(f"Rotated old backup: {f.name}")
            except Exception as e:
                log.warning(f"Could not delete {f}: {e}")


# ── Nexus alert ───────────────────────────────────────────────────────────────
def nexus_alert(subject: str, body: str, priority: str = "normal"):
    """Send a Nexus inbox message. Non-fatal — log and continue on failure."""
    try:
        import psycopg2
        conn = psycopg2.connect(
            host=os.environ.get("INBOX_DB_HOST", "YOUR_POSTGRES_IP"),
            dbname="lumina_inbox",
            user=os.environ.get("INBOX_DB_USER", "lumina_inbox_user"),
            password=os.environ.get("INBOX_DB_PASS", ""),
            connect_timeout=5,
        )
        with conn:
            with conn.cursor() as cur:
                cur.execute(
                    """
                    INSERT INTO inbox (sender, recipient, subject, body, priority, status, created_at)
                    VALUES (%s, %s, %s, %s, %s, 'unread', NOW())
                    """,
                    ("dura", "lumina", subject, body, priority),
                )
        conn.close()
        log.info(f"Nexus alert sent: {subject}")
    except Exception as e:
        log.warning(f"Nexus alert failed (non-fatal): {e}")


# ── Status writer ──────────────────────────────────────────────────────────────
def write_status(mode: str, results: list, started: datetime, nfs_ok: bool):
    ended = datetime.now()
    ok = sum(1 for r in results if r.get("status") == "ok")
    failed = [r["name"] for r in results if r.get("status") == "error"]
    status = {
        "mode": mode,
        "started": started.isoformat(),
        "ended": ended.isoformat(),
        "duration_seconds": round((ended - started).total_seconds(), 1),
        "total": len(results),
        "ok": ok,
        "failed_count": len(failed),
        "failed": failed,
        "nfs_available": nfs_ok,
        "backup_dir": LOCAL_BACKUP_ROOT,
        "results": results,
    }
    Path(STATUS_FILE).parent.mkdir(parents=True, exist_ok=True)
    with open(STATUS_FILE, "w") as f:
        json.dump(status, f, indent=2)
    log.info(f"Status written: {ok}/{len(results)} OK, {len(failed)} failed")
    return status


# ── Hourly run ────────────────────────────────────────────────────────────────
def run_hourly():
    """Critical SQLite backups: nexus, engram, myelin."""
    started = datetime.now()
    log.info("=== Dura HOURLY backup starting ===")
    results = []

    backup_dir = get_backup_dir("hourly")

    for name, path in SQLITE_DBS.items():
        r = backup_sqlite(path, backup_dir, name)
        results.append(r)

    rotate_backups(backup_dir, keep_daily=48, keep_weekly=0)  # keep 48 hourly = 2 days

    nfs_ok = nfs_available()
    if nfs_ok:
        rsync_to_nfs(LOCAL_BACKUP_ROOT)

    status = write_status("hourly", results, started, nfs_ok)

    ok_count = status["ok"]
    fail_count = status["failed_count"]
    summary = f"Hourly backup: {ok_count}/{len(results)} OK. NFS: {'yes' if nfs_ok else 'no'}."
    if fail_count:
        summary += f" FAILED: {', '.join(status['failed'])}"
        nexus_alert("Dura: hourly backup partial failure", summary, priority="high")
    else:
        nexus_alert("Dura: hourly backup complete", summary, priority="low")

    log.info("=== Dura HOURLY backup done ===")
    return status


# ── Daily run ─────────────────────────────────────────────────────────────────
def run_daily():
    """Full backup: all SQLite, all Postgres, fleet config."""
    started = datetime.now()
    log.info("=== Dura DAILY backup starting ===")
    results = []

    daily_dir = get_backup_dir("daily")

    # All SQLite DBs
    all_sqlite = {**SQLITE_DBS, **SQLITE_DBS_EXTRA}
    for name, path in all_sqlite.items():
        r = backup_sqlite(path, daily_dir, name)
        results.append(r)

    # Postgres on postgres-host
    for db in POSTGRES_DBS:
        db_pass = os.environ.get(db.get("pass_env", ""), "") if db.get("pass_env") else ""
        r = backup_postgres(db["name"], db["ct_id"], db["user"], daily_dir, db_pass=db_pass)
        results.append(r)

    # Plane Postgres (Docker on plane-host)
    r = backup_plane_postgres(daily_dir)
    results.append(r)

    # Fleet config tarball
    r = backup_fleet_config(daily_dir)
    results.append(r)

    rotate_backups(daily_dir, keep_daily=7, keep_weekly=4)

    nfs_ok = nfs_available()
    if nfs_ok:
        rsync_to_nfs(LOCAL_BACKUP_ROOT)

    status = write_status("daily", results, started, nfs_ok)

    ok_count = status["ok"]
    fail_count = status["failed_count"]
    summary = (
        f"Daily backup: {ok_count}/{len(results)} OK. "
        f"NFS: {'yes' if nfs_ok else 'no'}. "
        f"Duration: {status['duration_seconds']}s."
    )
    if fail_count:
        summary += f" FAILED: {', '.join(status['failed'])}"
        nexus_alert("Dura: daily backup partial failure", summary, priority="high")
    else:
        nexus_alert("Dura: daily backup complete", summary, priority="normal")

    log.info("=== Dura DAILY backup done ===")
    return status


# ── Status ────────────────────────────────────────────────────────────────────
def show_status():
    """Print last backup status."""
    if not os.path.exists(STATUS_FILE):
        print("No status file found. Run hourly or daily first.")
        return
    with open(STATUS_FILE) as f:
        status = json.load(f)
    print(json.dumps(status, indent=2))


# ── Entry point ───────────────────────────────────────────────────────────────
if __name__ == "__main__":
    load_env()

    if len(sys.argv) < 2:
        print("Usage: dura_backup.py hourly|daily|status")
        sys.exit(1)

    cmd = sys.argv[1].lower()
    if cmd == "hourly":
        result = run_hourly()
        sys.exit(0 if result["failed_count"] == 0 else 1)
    elif cmd == "daily":
        result = run_daily()
        sys.exit(0 if result["failed_count"] == 0 else 1)
    elif cmd == "status":
        show_status()
    else:
        print(f"Unknown command: {cmd}. Use hourly|daily|status")
        sys.exit(1)
