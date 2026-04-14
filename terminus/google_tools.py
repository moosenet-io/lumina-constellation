import os
import imaplib
import smtplib
import email
import json
import urllib.request
from email.mime.text import MIMEText
from email.mime.multipart import MIMEMultipart
from datetime import datetime, date, timedelta
from typing import Optional

# ============================================================
# Google Tools — Calendar (CalDAV) + Email (IMAP/SMTP)
# Uses App Password + standard protocols. No OAuth needed.
# Works with any Gmail account. Compatible with other CalDAV/IMAP providers.
# ============================================================

GOOGLE_EMAIL = os.environ.get('GOOGLE_LUMINA_EMAIL', '')
GOOGLE_PASSWORD = os.environ.get('GOOGLE_APP_PASSWORD', '')
LITELLM_URL = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
# Additional calendars to always include (comma-separated email IDs or group calendar IDs)
# the operator's personal calendar is shared with lumina account and accessible via CalDAV
GOOGLE_EXTRA_CALENDARS = [
    c.strip() for c in os.environ.get('GOOGLE_EXTRA_CALENDARS', '').split(',') if c.strip()
]
# Always include the operator's personal calendar and Lumina Actions
_PETER_EMAIL = os.environ.get('GOOGLE_PETER_EMAIL', '<operator-personal-email>')
_LUMINA_ACTIONS_ID = os.environ.get('GOOGLE_LUMINA_CALENDAR_ID', '')
# Build complete list of calendar IDs to query
_ALL_CALENDAR_IDS = [_PETER_EMAIL] + GOOGLE_EXTRA_CALENDARS
if _LUMINA_ACTIONS_ID:
    _ALL_CALENDAR_IDS.append(_LUMINA_ACTIONS_ID)


def _check_creds():
    if not GOOGLE_EMAIL or not GOOGLE_PASSWORD:
        return False, 'Google credentials not set. Check GOOGLE_LUMINA_EMAIL and GOOGLE_APP_PASSWORD in .env.'
    return True, ''


def _get_caldav_client():
    import caldav
    ok, err = _check_creds()
    if not ok:
        raise ValueError(err)
    # Google CalDAV requires per-user URL format
    caldav_url = f'https://www.google.com/calendar/dav/{GOOGLE_EMAIL}/events'
    client = caldav.DAVClient(
        url=caldav_url,
        username=GOOGLE_EMAIL,
        password=GOOGLE_PASSWORD,
    )
    return client


def _get_imap():
    ok, err = _check_creds()
    if not ok:
        raise ValueError(err)
    mail = imaplib.IMAP4_SSL('imap.gmail.com', 993)
    mail.login(GOOGLE_EMAIL, GOOGLE_PASSWORD)
    return mail


def register_google_tools(mcp):

    @mcp.tool()
    def google_calendar_today() -> dict:
        """Get today's calendar events from ALL calendars: Lumina's own + the operator's shared personal calendar.
        Queries lumina-assistant@your-domain.com (own), <operator-personal-email> (the operator's personal, shared), and Lumina Actions group calendar.
        Returns list of events with title, time, location, description, calendar name."""
        try:
            import caldav as _caldav
            ok, err = _check_creds()
            if not ok:
                return {'error': err}
            today_start = datetime.combine(date.today(), datetime.min.time())
            today_end = datetime.combine(date.today(), datetime.max.time())
            all_events = []
            seen_uids = set()  # dedup events that appear in multiple calendars

            # Query Lumina's own calendar first (always)
            own_cal_ids = [GOOGLE_EMAIL] + _ALL_CALENDAR_IDS
            # Deduplicate
            queried = set()
            for cal_id in own_cal_ids:
                if cal_id in queried:
                    continue
                queried.add(cal_id)
                try:
                    cal_url = f'https://www.google.com/calendar/dav/{cal_id}/events'
                    client = _caldav.DAVClient(url=cal_url, username=GOOGLE_EMAIL, password=GOOGLE_PASSWORD)
                    cal = _caldav.Calendar(client=client, url=cal_url)
                    events = cal.date_search(start=today_start, end=today_end, expand=True)
                    for event in events:
                        comp = event.icalendar_component
                        uid = str(comp.get('UID', ''))
                        if uid and uid in seen_uids:
                            continue
                        if uid:
                            seen_uids.add(uid)
                        dtstart = comp.get('DTSTART')
                        dtend = comp.get('DTEND')
                        # Use cal_id short name for display
                        cal_name = cal_id.split('@')[0] if '@' in cal_id else cal_id[:20]
                        all_events.append({
                            'title': str(comp.get('SUMMARY', 'No title')),
                            'start': str(dtstart.dt) if dtstart else '',
                            'end': str(dtend.dt) if dtend else '',
                            'location': str(comp.get('LOCATION', '')),
                            'description': str(comp.get('DESCRIPTION', ''))[:200],
                            'calendar': cal_name,
                        })
                except Exception:
                    pass  # Skip inaccessible calendars silently

            all_events.sort(key=lambda x: x.get('start', ''))
            return {'date': date.today().isoformat(), 'count': len(all_events), 'events': all_events}
        except Exception as e:
            return {'error': str(e)}

    @mcp.tool()
    def google_calendar_week(start_date: str = '') -> dict:
        """Get calendar events for a week from ALL calendars (Lumina's own + the operator's shared personal calendar).
        start_date: YYYY-MM-DD, default today. Queries <operator-personal-email> (the operator's personal) + Lumina Actions."""
        try:
            import caldav as _caldav
            ok, err = _check_creds()
            if not ok:
                return {'error': err}
            start = datetime.strptime(start_date, '%Y-%m-%d') if start_date else datetime.combine(date.today(), datetime.min.time())
            end = start + timedelta(days=7)
            all_events = []
            seen_uids = set()

            own_cal_ids = [GOOGLE_EMAIL] + _ALL_CALENDAR_IDS
            queried = set()
            for cal_id in own_cal_ids:
                if cal_id in queried:
                    continue
                queried.add(cal_id)
                try:
                    cal_url = f'https://www.google.com/calendar/dav/{cal_id}/events'
                    client = _caldav.DAVClient(url=cal_url, username=GOOGLE_EMAIL, password=GOOGLE_PASSWORD)
                    cal = _caldav.Calendar(client=client, url=cal_url)
                    events = cal.date_search(start=start, end=end, expand=True)
                    for event in events:
                        comp = event.icalendar_component
                        uid = str(comp.get('UID', ''))
                        if uid and uid in seen_uids:
                            continue
                        if uid:
                            seen_uids.add(uid)
                        dtstart = comp.get('DTSTART')
                        cal_name = cal_id.split('@')[0] if '@' in cal_id else cal_id[:20]
                        all_events.append({
                            'title': str(comp.get('SUMMARY', 'No title')),
                            'start': str(dtstart.dt) if dtstart else '',
                            'location': str(comp.get('LOCATION', '')),
                            'calendar': cal_name,
                        })
                except Exception:
                    pass

            all_events.sort(key=lambda x: x.get('start', ''))
            return {'week_start': start.date().isoformat(), 'count': len(all_events), 'events': all_events}
        except Exception as e:
            return {'error': str(e)}

    @mcp.tool()
    def google_calendar_add(
        title: str,
        start: str,
        end: str,
        description: str = '',
        location: str = '',
    ) -> dict:
        """Create a calendar event on the Lumina Actions calendar.
        start/end: ISO8601 format e.g. '2026-04-07T14:00:00'
        Returns: {status, event_url}"""
        try:
            from icalendar import Calendar, Event
            import uuid
            
            client = _get_caldav_client()
            principal = client.principal()
            
            # Find or use Lumina Actions calendar
            calendars = principal.calendars()
            target_cal = None
            for cal in calendars:
                if 'lumina' in (cal.name or '').lower() or 'actions' in (cal.name or '').lower():
                    target_cal = cal
                    break
            if target_cal is None and calendars:
                target_cal = calendars[0]  # fallback to first calendar
            
            if not target_cal:
                return {'error': 'No calendar found to write to'}
            
            cal = Calendar()
            cal.add('prodid', '-//Lumina//Constellation//EN')
            cal.add('version', '2.0')
            
            evt = Event()
            evt.add('summary', title)
            evt.add('dtstart', datetime.fromisoformat(start))
            evt.add('dtend', datetime.fromisoformat(end))
            if description:
                evt.add('description', description)
            if location:
                evt.add('location', location)
            evt.add('uid', str(uuid.uuid4()))
            cal.add_component(evt)
            
            target_cal.save_event(cal.to_ical())
            return {'status': 'created', 'title': title, 'start': start, 'calendar': target_cal.name}
        except Exception as e:
            return {'error': str(e)}

    @mcp.tool()
    def google_calendar_conflicts(start: str, end: str) -> dict:
        """Check if a time slot has any calendar conflicts.
        start/end: ISO8601 format. Returns list of conflicting events."""
        try:
            client = _get_caldav_client()
            principal = client.principal()
            start_dt = datetime.fromisoformat(start)
            end_dt = datetime.fromisoformat(end)
            
            conflicts = []
            for cal in principal.calendars():
                try:
                    events = cal.date_search(start=start_dt, end=end_dt, expand=True)
                    for event in events:
                        comp = event.icalendar_component
                        conflicts.append({
                            'title': str(comp.get('SUMMARY', 'Busy')),
                            'calendar': cal.name,
                        })
                except Exception:
                    pass
            
            return {'has_conflicts': len(conflicts) > 0, 'count': len(conflicts), 'conflicts': conflicts}
        except Exception as e:
            return {'error': str(e)}

    @mcp.tool()
    def google_email_inbox(limit: int = 10, unread_only: bool = False) -> dict:
        """List recent emails in Lumina's inbox.
        limit: max emails to return. unread_only: only show unread."""
        try:
            mail = _get_imap()
            mail.select('INBOX')
            
            search_criteria = 'UNSEEN' if unread_only else 'ALL'
            _, data = mail.search(None, search_criteria)
            ids = data[0].split()
            
            # Get most recent N emails
            recent_ids = ids[-limit:] if len(ids) > limit else ids
            recent_ids = list(reversed(recent_ids))  # newest first
            
            emails = []
            for eid in recent_ids:
                _, msg_data = mail.fetch(eid, '(RFC822.SIZE BODY[HEADER.FIELDS (FROM SUBJECT DATE)])')
                if msg_data and msg_data[0]:
                    headers = email.message_from_bytes(msg_data[0][1])
                    emails.append({
                        'id': eid.decode(),
                        'from': headers.get('From', ''),
                        'subject': headers.get('Subject', ''),
                        'date': headers.get('Date', ''),
                    })
            
            mail.logout()
            return {'count': len(emails), 'total_in_inbox': len(ids), 'emails': emails}
        except Exception as e:
            return {'error': str(e)}

    @mcp.tool()
    def google_email_read(email_id: str) -> dict:
        """Read the full content of an email. email_id: from google_email_inbox()."""
        try:
            mail = _get_imap()
            mail.select('INBOX')
            _, msg_data = mail.fetch(email_id.encode(), '(RFC822)')
            if not msg_data or not msg_data[0]:
                mail.logout()
                return {'error': f'Email {email_id} not found'}
            
            msg = email.message_from_bytes(msg_data[0][1])
            body = ''
            if msg.is_multipart():
                for part in msg.walk():
                    if part.get_content_type() == 'text/plain':
                        body = part.get_payload(decode=True).decode('utf-8', errors='replace')[:3000]
                        break
            else:
                body = msg.get_payload(decode=True).decode('utf-8', errors='replace')[:3000]
            
            mail.logout()
            return {
                'from': msg.get('From', ''),
                'to': msg.get('To', ''),
                'subject': msg.get('Subject', ''),
                'date': msg.get('Date', ''),
                'body': body,
            }
        except Exception as e:
            return {'error': str(e)}

    @mcp.tool()
    def google_email_send(to: str, subject: str, body: str) -> dict:
        """Send an email from Lumina's account (lumina-assistant@your-domain.com).
        to: recipient email. subject: email subject. body: plain text body."""
        try:
            ok, err = _check_creds()
            if not ok:
                return {'error': err}
            
            msg = MIMEMultipart()
            msg['From'] = GOOGLE_EMAIL
            msg['To'] = to
            msg['Subject'] = subject
            msg.attach(MIMEText(body, 'plain'))
            
            with smtplib.SMTP('smtp.gmail.com', 587) as server:
                server.starttls()
                server.login(GOOGLE_EMAIL, GOOGLE_PASSWORD)
                server.send_message(msg)
            
            return {'status': 'sent', 'to': to, 'subject': subject}
        except Exception as e:
            return {'error': str(e)}

    @mcp.tool()
    def google_email_summary(hours_back: int = 12) -> dict:
        """Get a Qwen-summarized summary of recent inbox activity.
        hours_back: look at emails from last N hours (default 12)."""
        try:
            # Get recent emails
            mail = _get_imap()
            mail.select('INBOX')
            
            from datetime import timezone
            since = (datetime.now() - timedelta(hours=hours_back)).strftime('%d-%b-%Y')
            _, data = mail.search(None, f'SINCE {since}')
            ids = data[0].split()[-20:]  # max 20 recent
            
            subjects = []
            for eid in reversed(ids):
                _, msg_data = mail.fetch(eid, '(BODY[HEADER.FIELDS (FROM SUBJECT DATE)])')
                if msg_data and msg_data[0]:
                    headers = email.message_from_bytes(msg_data[0][1])
                    subjects.append(f"From: {headers.get('From','?')[:40]} | {headers.get('Subject','?')[:60]}")
            
            mail.logout()
            
            if not subjects:
                return {'summary': f'No emails in the last {hours_back} hours.', 'count': 0}
            
            # Qwen summary
            prompt = f'Summarize this inbox activity in 2-3 sentences. Focus on what needs attention:\n\n' + '\n'.join(subjects[:10])
            try:
                data = json.dumps({'model': 'Lumina Fast', 'messages': [{'role': 'user', 'content': prompt}], 'max_tokens': 150}).encode()
                req = urllib.request.Request(f'{LITELLM_URL}/v1/chat/completions', data=data,
                    headers={'Authorization': f'Bearer {LITELLM_KEY}', 'Content-Type': 'application/json'}, method='POST')
                with urllib.request.urlopen(req, timeout=20) as r:
                    summary = json.load(r)['choices'][0]['message']['content']
            except Exception:
                summary = f'{len(subjects)} emails received. Subjects: ' + '; '.join(s.split('|')[1].strip() for s in subjects[:5])
            
            return {'summary': summary, 'count': len(subjects), 'hours_back': hours_back}
        except Exception as e:
            return {'error': str(e)}
