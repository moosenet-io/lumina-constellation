#!/usr/bin/env python3
"""
scheduler.py — APScheduler-based task runner for containerised fleet. (DT.10)

Replaces systemd timers when running in Docker Compose. All fleet tasks
that previously ran via systemd units now run here.

Usage:
    python3 scheduler.py       # Start scheduler (blocks until SIGTERM)
    python3 scheduler.py --list  # List all registered jobs and exit

Environment:
    LUMINA_TIMEZONE  IANA timezone for cron triggers (default: America/Los_Angeles)
    FLEET_DIR        Path to fleet directory (default: /opt/lumina-fleet)
"""

import logging
import os
import signal
import sys
import time
from pathlib import Path
from datetime import datetime

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [scheduler] %(levelname)s %(message)s",
    datefmt="%Y-%m-%dT%H:%M:%S",
)
log = logging.getLogger("scheduler")

try:
    from apscheduler.schedulers.blocking import BlockingScheduler
    from apscheduler.triggers.cron import CronTrigger
    from apscheduler.triggers.interval import IntervalTrigger
    from apscheduler.events import EVENT_JOB_ERROR, EVENT_JOB_EXECUTED
except ImportError:
    log.error("APScheduler not installed. Run: pip install apscheduler")
    sys.exit(1)

FLEET_DIR = Path(os.environ.get("FLEET_DIR", "/opt/lumina-fleet"))
TIMEZONE = os.environ.get("LUMINA_TIMEZONE", "America/Los_Angeles")
DATA_DIR = Path(os.environ.get("SPECTRA_DATA", "/data"))
ALIVE_FILE = DATA_DIR / ".scheduler-alive"


# ── Heartbeat ─────────────────────────────────────────────────────────────────

def _heartbeat():
    """Write alive file (checked by Docker healthcheck)."""
    try:
        ALIVE_FILE.parent.mkdir(parents=True, exist_ok=True)
        ALIVE_FILE.write_text(datetime.utcnow().isoformat())
    except Exception as e:
        log.warning(f"Heartbeat write failed: {e}")


# ── Task wrappers ─────────────────────────────────────────────────────────────

def run_vigil_morning():
    log.info("→ Vigil: morning briefing")
    try:
        sys.path.insert(0, str(FLEET_DIR))
        briefing = __import__("vigil.briefing", fromlist=["run_briefing"])
        briefing.run_briefing("morning")
    except Exception as e:
        log.error(f"Vigil morning failed: {e}")


def run_vigil_afternoon():
    log.info("→ Vigil: afternoon briefing")
    try:
        sys.path.insert(0, str(FLEET_DIR))
        briefing = __import__("vigil.briefing", fromlist=["run_briefing"])
        briefing.run_briefing("afternoon")
    except Exception as e:
        log.error(f"Vigil afternoon failed: {e}")


def run_sentinel_health():
    log.info("→ Sentinel: health checks")
    try:
        import subprocess
        result = subprocess.run(
            [sys.executable, str(FLEET_DIR / "sentinel" / "health_checks.py")],
            capture_output=True, text=True, timeout=120
        )
        if result.returncode not in (0, 1):
            log.error(f"Sentinel health check failed (exit {result.returncode}): {result.stderr[:200]}")
        else:
            log.info(f"Sentinel health check complete (exit {result.returncode})")
    except Exception as e:
        log.error(f"Sentinel health check error: {e}")


def run_sentinel_alerts():
    log.info("→ Sentinel: alert evaluation")
    try:
        import subprocess
        result = subprocess.run(
            [sys.executable, str(FLEET_DIR / "sentinel" / "alert_rules.py")],
            capture_output=True, text=True, timeout=30
        )
        log.info(f"Sentinel alerts complete (exit {result.returncode})")
    except Exception as e:
        log.error(f"Sentinel alert error: {e}")


def run_axon_poll():
    log.info("→ Axon: poll Nexus inbox")
    try:
        import subprocess
        result = subprocess.run(
            [sys.executable, str(FLEET_DIR / "axon" / "axon.py"), "--once"],
            capture_output=True, text=True, timeout=120
        )
        if result.stdout.strip():
            log.info(f"Axon: {result.stdout.strip()[:200]}")
    except Exception as e:
        log.error(f"Axon poll error: {e}")


def run_synapse_scan():
    log.info("→ Synapse: scanner run")
    try:
        import subprocess
        result = subprocess.run(
            [sys.executable, str(FLEET_DIR / "synapse" / "synapse_scan.py")],
            capture_output=True, text=True, timeout=60
        )
        log.info(f"Synapse scan complete (exit {result.returncode})")
    except Exception as e:
        log.error(f"Synapse scan error: {e}")


def run_spectra_cleanup():
    log.info("→ Spectra: cleanup old recordings/screenshots")
    try:
        import subprocess
        result = subprocess.run(
            [sys.executable, "-c",
             "import sys,os,time; "
             "from pathlib import Path; "
             "data=Path('/data/spectra'); "
             "[f.unlink() for d in ['recordings','screenshots'] "
             " for f in (data/d).glob('*') if f.is_file() "
             " and time.time()-f.stat().st_mtime > 7*86400]"],
            capture_output=True, text=True, timeout=30
        )
        log.info("Spectra cleanup complete")
    except Exception as e:
        log.error(f"Spectra cleanup error: {e}")


def run_skill_evolution():
    log.info("→ Skills: evolution tick")
    try:
        import subprocess
        skill_tracker = FLEET_DIR / "shared" / "skill_tracker.py"
        if skill_tracker.exists():
            result = subprocess.run(
                [sys.executable, str(skill_tracker), "--tick"],
                capture_output=True, text=True, timeout=60
            )
            log.info(f"Skill evolution complete (exit {result.returncode})")
    except Exception as e:
        log.error(f"Skill evolution error: {e}")


def run_myelin_collect():
    log.info("→ Myelin: cost data collection")
    try:
        import subprocess
        myelin = FLEET_DIR / "myelin" / "myelin_collect.py"
        if myelin.exists():
            subprocess.run([sys.executable, str(myelin)],
                          capture_output=True, text=True, timeout=30)
    except Exception as e:
        log.error(f"Myelin collect error: {e}")


def run_secret_rotation_check():
    log.info("→ Security: secret rotation check")
    try:
        import subprocess
        rotation = FLEET_DIR / "security" / "rotation.py"
        if rotation.exists():
            result = subprocess.run(
                [sys.executable, str(rotation), "run"],
                capture_output=True, text=True, timeout=120
            )
            log.info(f"Rotation check complete (exit {result.returncode})")
    except Exception as e:
        log.error(f"Secret rotation error: {e}")


# ── Scheduler factory ─────────────────────────────────────────────────────────

def create_scheduler() -> BlockingScheduler:
    """Create and configure the APScheduler with all Lumina jobs."""
    tz = TIMEZONE
    scheduler = BlockingScheduler(timezone=tz)

    # Heartbeat (every 30s — for Docker healthcheck)
    scheduler.add_job(
        _heartbeat, IntervalTrigger(seconds=30),
        id="heartbeat", name="Scheduler heartbeat",
    )

    # Vigil morning briefing — 7:00 AM operator timezone
    scheduler.add_job(
        run_vigil_morning, CronTrigger(hour=7, minute=0, timezone=tz),
        id="vigil_morning", name="Vigil morning briefing",
    )

    # Vigil afternoon check-in — 5:00 PM
    scheduler.add_job(
        run_vigil_afternoon, CronTrigger(hour=17, minute=0, timezone=tz),
        id="vigil_afternoon", name="Vigil afternoon briefing",
    )

    # Sentinel health checks — every 30 minutes
    scheduler.add_job(
        run_sentinel_health, IntervalTrigger(minutes=30),
        id="sentinel_health", name="Sentinel health checks",
    )

    # Sentinel alert evaluation — every 30 minutes (offset by 5 min)
    scheduler.add_job(
        run_sentinel_alerts, IntervalTrigger(minutes=30, start_date="2000-01-01 00:05:00"),
        id="sentinel_alerts", name="Sentinel alert rules",
    )

    # Axon poll — every 60 seconds
    scheduler.add_job(
        run_axon_poll, IntervalTrigger(seconds=60),
        id="axon_poll", name="Axon Nexus inbox poll",
    )

    # Synapse scan — every 30 minutes
    scheduler.add_job(
        run_synapse_scan, IntervalTrigger(minutes=30),
        id="synapse_scan", name="Synapse spontaneous scan",
    )

    # Spectra cleanup — daily at 2:00 AM
    scheduler.add_job(
        run_spectra_cleanup, CronTrigger(hour=2, minute=0, timezone=tz),
        id="spectra_cleanup", name="Spectra recording/screenshot cleanup",
    )

    # Skill evolution — daily at 2:30 AM
    scheduler.add_job(
        run_skill_evolution, CronTrigger(hour=2, minute=30, timezone=tz),
        id="skill_evolution", name="Skill evolution tick",
    )

    # Myelin cost collection — every 15 minutes
    scheduler.add_job(
        run_myelin_collect, IntervalTrigger(minutes=15),
        id="myelin_collect", name="Myelin cost data collection",
    )

    # Secret rotation check — daily at 8:17 AM (as per DT spec)
    scheduler.add_job(
        run_secret_rotation_check, CronTrigger(hour=8, minute=17, timezone=tz),
        id="secret_rotation", name="Secret rotation check",
    )

    return scheduler


# ── Event handlers ────────────────────────────────────────────────────────────

def _on_job_executed(event):
    if event.job_id != "heartbeat":
        log.debug(f"Job completed: {event.job_id}")


def _on_job_error(event):
    log.error(f"Job failed: {event.job_id} — {event.exception}")


# ── Main ──────────────────────────────────────────────────────────────────────

def main():
    if "--list" in sys.argv:
        s = create_scheduler()
        print(f"\nScheduled jobs ({len(s.get_jobs())} total):\n")
        for job in s.get_jobs():
            print(f"  {job.id:<25} {job.name:<40} {job.trigger}")
        return

    log.info(f"Lumina Scheduler starting (timezone: {TIMEZONE})")
    log.info(f"Fleet dir: {FLEET_DIR}")

    scheduler = create_scheduler()
    scheduler.add_listener(_on_job_executed, EVENT_JOB_EXECUTED)
    scheduler.add_listener(_on_job_error, EVENT_JOB_ERROR)

    # Graceful shutdown on SIGTERM / SIGINT
    def _shutdown(signum, frame):
        log.info("SIGTERM received — shutting down scheduler")
        scheduler.shutdown(wait=False)
        sys.exit(0)

    signal.signal(signal.SIGTERM, _shutdown)
    signal.signal(signal.SIGINT, _shutdown)

    log.info(f"Scheduler started with {len(scheduler.get_jobs())} jobs")
    _heartbeat()

    try:
        scheduler.start()
    except Exception as e:
        log.error(f"Scheduler error: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
