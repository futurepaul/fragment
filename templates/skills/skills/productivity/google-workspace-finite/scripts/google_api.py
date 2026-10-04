#!/usr/bin/env python3
"""Google Workspace API CLI, through the person's connected Google account.

Rewritten for fragment from Hermes Agent's Google Workspace helper (MIT,
Nous Research): the same commands and answers, over Google's REST APIs with
Python's standard library alone. The computer holds no Google credential:
its environment holds the connection's placeholder, which names the agent
(`GOOGLE_OAUTH_ACCESS_TOKEN`), each request sends it as a bearer token, and
the computer's intercept swaps in a short-lived token for the agent's owner
(docs/computers.md, Connections and operator keys).

Usage:
  python google_api.py gmail search "is:unread" [--max 10]
  python google_api.py gmail get MESSAGE_ID
  python google_api.py gmail send --to user@example.com --subject "Hi" --body "Hello"
  python google_api.py gmail reply MESSAGE_ID --body "Thanks"
  python google_api.py gmail labels
  python google_api.py gmail modify MESSAGE_ID --add-labels L1 --remove-labels UNREAD
  python google_api.py calendar list [--start ISO] [--end ISO] [--calendar primary]
  python google_api.py calendar create --summary "Meeting" --start ISO --end ISO
  python google_api.py calendar delete EVENT_ID
  python google_api.py drive search "budget report" [--max 10] [--raw-query]
  python google_api.py contacts list [--max 20]
  python google_api.py sheets get SHEET_ID RANGE
  python google_api.py sheets update SHEET_ID RANGE --values '[[...]]'
  python google_api.py sheets append SHEET_ID RANGE --values '[[...]]'
  python google_api.py docs get DOC_ID
  python google_api.py check

Environment:
  GOOGLE_OAUTH_ACCESS_TOKEN  the Google connection's placeholder (set on a
                             fragment computer once the person connects
                             Google), or any Google OAuth access token
"""

import argparse
import base64
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone
from email.mime.text import MIMEText

GMAIL = "https://gmail.googleapis.com/gmail/v1/users/me"
CALENDAR = "https://www.googleapis.com/calendar/v3"
DRIVE = "https://www.googleapis.com/drive/v3"
PEOPLE = "https://people.googleapis.com/v1"
SHEETS = "https://sheets.googleapis.com/v4/spreadsheets"
DOCS = "https://docs.googleapis.com/v1/documents"

# A search reads at most this many messages' metadata, one request each.
SEARCH_MAX = 50
TIMEOUT_S = 60


class GoogleError(Exception):
    def __init__(self, status, code, message):
        super().__init__(message)
        self.status = status
        self.code = code


def headers(extra=None):
    token = os.getenv("GOOGLE_OAUTH_ACCESS_TOKEN", "").strip()
    if not token:
        raise GoogleError(0, "not_connected", "GOOGLE_OAUTH_ACCESS_TOKEN is not set: the person has not connected Google, or narrowed you from it")
    h = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
    h.update(extra or {})
    return h


def call(method, url, params=None, body=None):
    if params:
        url = f"{url}?{urllib.parse.urlencode(params, doseq=True)}"
    data = None
    extra = {}
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        extra["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers(extra), method=method)
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_S) as response:
            raw = response.read()
    except urllib.error.HTTPError as exc:
        text = exc.read().decode("utf-8", errors="replace")
        try:
            err = json.loads(text)
        except ValueError:
            err = {}
        e = err.get("error") if isinstance(err, dict) else None
        if isinstance(e, str):
            # the computer's own refusal names its code: not_connected, forbidden
            code, message = e, err.get("message")
        elif isinstance(e, dict):
            # Google's: {error: {code, message, status}}
            code, message = e.get("status", ""), e.get("message")
        else:
            code, message = "", None
        raise GoogleError(exc.code, code or "", message or text[:500] or f"HTTP {exc.code}") from None
    except urllib.error.URLError as exc:
        raise GoogleError(0, "unreachable", str(exc.reason)) from None
    if not raw:
        return {}
    return json.loads(raw.decode("utf-8"))


def out(value):
    print(json.dumps(value, indent=2, ensure_ascii=False))


def header_map(message):
    return {h["name"]: h["value"] for h in message.get("payload", {}).get("headers", [])}


def decode_part(data):
    return base64.urlsafe_b64decode(data + "=" * (-len(data) % 4)).decode("utf-8", errors="replace")


def body_text(payload):
    if payload.get("body", {}).get("data"):
        return decode_part(payload["body"]["data"])
    parts = payload.get("parts") or []
    for mime in ("text/plain", "text/html"):
        for part in parts:
            if part.get("mimeType") == mime and part.get("body", {}).get("data"):
                return decode_part(part["body"]["data"])
    for part in parts:
        nested = body_text(part)
        if nested:
            return nested
    return ""


# ---- Gmail ----


def gmail_search(args):
    listed = call("GET", f"{GMAIL}/messages", {"q": args.query, "maxResults": min(args.max, SEARCH_MAX)})
    messages = listed.get("messages", [])
    if not messages:
        print("No messages found.")
        return
    rows = []
    for meta in messages:
        msg = call("GET", f"{GMAIL}/messages/{meta['id']}", {"format": "metadata", "metadataHeaders": ["From", "To", "Subject", "Date"]})
        h = header_map(msg)
        rows.append({"id": msg["id"], "threadId": msg.get("threadId", ""), "from": h.get("From", ""), "to": h.get("To", ""), "subject": h.get("Subject", ""), "date": h.get("Date", ""), "snippet": msg.get("snippet", ""), "labels": msg.get("labelIds", [])})
    out(rows)


def gmail_get(args):
    msg = call("GET", f"{GMAIL}/messages/{args.message_id}", {"format": "full"})
    h = header_map(msg)
    out({"id": msg["id"], "threadId": msg.get("threadId", ""), "from": h.get("From", ""), "to": h.get("To", ""), "subject": h.get("Subject", ""), "date": h.get("Date", ""), "labels": msg.get("labelIds", []), "body": body_text(msg.get("payload", {}))})


def send_raw(message, thread_id=""):
    body = {"raw": base64.urlsafe_b64encode(message.as_bytes()).decode()}
    if thread_id:
        body["threadId"] = thread_id
    result = call("POST", f"{GMAIL}/messages/send", body=body)
    out({"status": "sent", "id": result.get("id", ""), "threadId": result.get("threadId", "")})


def gmail_send(args):
    message = MIMEText(args.body, "html" if args.html else "plain")
    message["to"] = args.to
    message["subject"] = args.subject
    if args.cc:
        message["cc"] = args.cc
    send_raw(message, args.thread_id)


def gmail_reply(args):
    original = call("GET", f"{GMAIL}/messages/{args.message_id}", {"format": "metadata", "metadataHeaders": ["From", "Subject", "Message-ID"]})
    h = header_map(original)
    subject = h.get("Subject", "")
    if not subject.startswith("Re:"):
        subject = f"Re: {subject}"
    message = MIMEText(args.body)
    message["to"] = h.get("From", "")
    message["subject"] = subject
    if h.get("Message-ID"):
        message["In-Reply-To"] = h["Message-ID"]
        message["References"] = h["Message-ID"]
    send_raw(message, original.get("threadId", ""))


def gmail_labels(args):
    listed = call("GET", f"{GMAIL}/labels")
    out([{"id": label["id"], "name": label["name"], "type": label.get("type", "")} for label in listed.get("labels", [])])


def gmail_modify(args):
    body = {}
    if args.add_labels:
        body["addLabelIds"] = args.add_labels.split(",")
    if args.remove_labels:
        body["removeLabelIds"] = args.remove_labels.split(",")
    result = call("POST", f"{GMAIL}/messages/{args.message_id}/modify", body=body)
    out({"id": result.get("id", ""), "labels": result.get("labelIds", [])})


# ---- Calendar ----


def with_zone(value):
    if "T" in value and not value.endswith("Z") and "+" not in value and "-" not in value[11:]:
        return value + "Z"
    return value


def calendar_list(args):
    now = datetime.now(timezone.utc)
    params = {"timeMin": with_zone(args.start or now.isoformat()), "timeMax": with_zone(args.end or (now + timedelta(days=7)).isoformat()), "maxResults": args.max, "singleEvents": "true", "orderBy": "startTime"}
    listed = call("GET", f"{CALENDAR}/calendars/{urllib.parse.quote(args.calendar, safe='')}/events", params)
    events = []
    for e in listed.get("items", []):
        events.append({"id": e["id"], "summary": e.get("summary", "(no title)"), "start": e.get("start", {}).get("dateTime", e.get("start", {}).get("date", "")), "end": e.get("end", {}).get("dateTime", e.get("end", {}).get("date", "")), "location": e.get("location", ""), "description": e.get("description", ""), "status": e.get("status", ""), "htmlLink": e.get("htmlLink", "")})
    out(events)


def calendar_create(args):
    event = {"summary": args.summary, "start": {"dateTime": args.start}, "end": {"dateTime": args.end}}
    if args.location:
        event["location"] = args.location
    if args.description:
        event["description"] = args.description
    if args.attendees:
        event["attendees"] = [{"email": e.strip()} for e in args.attendees.split(",") if e.strip()]
    result = call("POST", f"{CALENDAR}/calendars/{urllib.parse.quote(args.calendar, safe='')}/events", body=event)
    out({"status": "created", "id": result.get("id", ""), "summary": result.get("summary", ""), "htmlLink": result.get("htmlLink", "")})


def calendar_delete(args):
    call("DELETE", f"{CALENDAR}/calendars/{urllib.parse.quote(args.calendar, safe='')}/events/{urllib.parse.quote(args.event_id, safe='')}")
    print(json.dumps({"status": "deleted", "eventId": args.event_id}))


# ---- Drive, Contacts, Sheets, Docs ----


def drive_search(args):
    query = args.query if args.raw_query else "fullText contains '{}'".format(args.query.replace("\\", "\\\\").replace("'", "\\'"))
    listed = call("GET", f"{DRIVE}/files", {"q": query, "pageSize": args.max, "fields": "files(id, name, mimeType, modifiedTime, webViewLink)"})
    out(listed.get("files", []))


def contacts_list(args):
    listed = call("GET", f"{PEOPLE}/people/me/connections", {"pageSize": args.max, "personFields": "names,emailAddresses,phoneNumbers"})
    contacts = []
    for person in listed.get("connections", []):
        names = person.get("names", [])
        contacts.append({"name": names[0].get("displayName", "") if names else "", "emails": [e.get("value", "") for e in person.get("emailAddresses", [])], "phones": [p.get("value", "") for p in person.get("phoneNumbers", [])]})
    out(contacts)


def sheets_get(args):
    result = call("GET", f"{SHEETS}/{args.sheet_id}/values/{urllib.parse.quote(args.range, safe='')}")
    out(result.get("values", []))


def sheets_update(args):
    result = call("PUT", f"{SHEETS}/{args.sheet_id}/values/{urllib.parse.quote(args.range, safe='')}", {"valueInputOption": "USER_ENTERED"}, {"values": json.loads(args.values)})
    out({"updatedCells": result.get("updatedCells", 0), "updatedRange": result.get("updatedRange", "")})


def sheets_append(args):
    result = call("POST", f"{SHEETS}/{args.sheet_id}/values/{urllib.parse.quote(args.range, safe='')}:append", {"valueInputOption": "USER_ENTERED", "insertDataOption": "INSERT_ROWS"}, {"values": json.loads(args.values)})
    out({"updatedCells": result.get("updates", {}).get("updatedCells", 0)})


def docs_get(args):
    doc = call("GET", f"{DOCS}/{args.doc_id}")
    text = []
    for element in doc.get("body", {}).get("content", []):
        for pe in element.get("paragraph", {}).get("elements", []):
            if pe.get("textRun", {}).get("content"):
                text.append(pe["textRun"]["content"])
    out({"title": doc.get("title", ""), "documentId": doc.get("documentId", ""), "body": "".join(text)})


def check(args):
    # one cheap read: whether the connection answers for this agent
    profile = call("GET", f"{GMAIL}/profile")
    out({"status": "connected", "email": profile.get("emailAddress", "")})


def main():
    parser = argparse.ArgumentParser(description="Google Workspace through the person's connected Google account")
    sub = parser.add_subparsers(dest="service", required=True)

    gmail = sub.add_parser("gmail")
    gmail_sub = gmail.add_subparsers(dest="action", required=True)
    p = gmail_sub.add_parser("search")
    p.add_argument("query", help="Gmail search query (e.g. 'is:unread')")
    p.add_argument("--max", type=int, default=10)
    p.set_defaults(func=gmail_search)
    p = gmail_sub.add_parser("get")
    p.add_argument("message_id")
    p.set_defaults(func=gmail_get)
    p = gmail_sub.add_parser("send")
    p.add_argument("--to", required=True)
    p.add_argument("--subject", required=True)
    p.add_argument("--body", required=True)
    p.add_argument("--cc", default="")
    p.add_argument("--html", action="store_true", help="Send body as HTML")
    p.add_argument("--thread-id", default="", help="Thread ID for threading")
    p.set_defaults(func=gmail_send)
    p = gmail_sub.add_parser("reply")
    p.add_argument("message_id", help="Message ID to reply to")
    p.add_argument("--body", required=True)
    p.set_defaults(func=gmail_reply)
    p = gmail_sub.add_parser("labels")
    p.set_defaults(func=gmail_labels)
    p = gmail_sub.add_parser("modify")
    p.add_argument("message_id")
    p.add_argument("--add-labels", default="", help="Comma-separated label IDs to add")
    p.add_argument("--remove-labels", default="", help="Comma-separated label IDs to remove")
    p.set_defaults(func=gmail_modify)

    cal = sub.add_parser("calendar")
    cal_sub = cal.add_subparsers(dest="action", required=True)
    p = cal_sub.add_parser("list")
    p.add_argument("--start", default="", help="Start time (ISO 8601)")
    p.add_argument("--end", default="", help="End time (ISO 8601)")
    p.add_argument("--max", type=int, default=25)
    p.add_argument("--calendar", default="primary")
    p.set_defaults(func=calendar_list)
    p = cal_sub.add_parser("create")
    p.add_argument("--summary", required=True)
    p.add_argument("--start", required=True, help="Start (ISO 8601 with timezone)")
    p.add_argument("--end", required=True, help="End (ISO 8601 with timezone)")
    p.add_argument("--location", default="")
    p.add_argument("--description", default="")
    p.add_argument("--attendees", default="", help="Comma-separated email addresses")
    p.add_argument("--calendar", default="primary")
    p.set_defaults(func=calendar_create)
    p = cal_sub.add_parser("delete")
    p.add_argument("event_id")
    p.add_argument("--calendar", default="primary")
    p.set_defaults(func=calendar_delete)

    drv = sub.add_parser("drive")
    drv_sub = drv.add_subparsers(dest="action", required=True)
    p = drv_sub.add_parser("search")
    p.add_argument("query")
    p.add_argument("--max", type=int, default=10)
    p.add_argument("--raw-query", action="store_true", help="Use query as a raw Drive query (e.g. mimeType='application/pdf')")
    p.set_defaults(func=drive_search)

    con = sub.add_parser("contacts")
    con_sub = con.add_subparsers(dest="action", required=True)
    p = con_sub.add_parser("list")
    p.add_argument("--max", type=int, default=50)
    p.set_defaults(func=contacts_list)

    sh = sub.add_parser("sheets")
    sh_sub = sh.add_subparsers(dest="action", required=True)
    p = sh_sub.add_parser("get")
    p.add_argument("sheet_id")
    p.add_argument("range")
    p.set_defaults(func=sheets_get)
    p = sh_sub.add_parser("update")
    p.add_argument("sheet_id")
    p.add_argument("range")
    p.add_argument("--values", required=True, help="JSON array of arrays")
    p.set_defaults(func=sheets_update)
    p = sh_sub.add_parser("append")
    p.add_argument("sheet_id")
    p.add_argument("range")
    p.add_argument("--values", required=True, help="JSON array of arrays")
    p.set_defaults(func=sheets_append)

    docs = sub.add_parser("docs")
    docs_sub = docs.add_subparsers(dest="action", required=True)
    p = docs_sub.add_parser("get")
    p.add_argument("doc_id")
    p.set_defaults(func=docs_get)

    p = sub.add_parser("check", help="whether the Google connection answers for this agent")
    p.set_defaults(func=check)

    args = parser.parse_args()
    try:
        args.func(args)
    except GoogleError as e:
        hint = {
            "not_connected": "the person has not connected Google (or must again): ask them to, in the shell's Settings, under Connections",
            "forbidden": "the person narrowed this agent's connections, or Google refused the scope",
        }.get(e.code, "")
        print(json.dumps({"error": e.code or "google_error", "status": e.status, "message": str(e), "hint": hint}), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
