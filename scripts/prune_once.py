"""One-shot log retention against the live database.

Applies exactly the two stages the background worker runs, at the same default
windows (bodies 7d, rows 30d), but synchronously so the freed pages are
visible to a follow-up VACUUM. Use once, then let the worker keep it bounded.

Server must be stopped: this writes to data/marionette.sqlite directly.
"""
import sqlite3
import sys

DB = "data/marionette.sqlite"
BATCH = 500


def days_ago(c, d):
    # Match the app's stored format: RFC3339 UTC with a trailing Z.
    return c.execute("select strftime('%Y-%m-%dT%H:%M:%S','now',?)", (f"-{d} days",)).fetchone()[0] + ".000Z"


def null_bodies(c, cutoff):
    total = 0
    while True:
        cur = c.execute(
            """
            UPDATE request_logs SET request_body=NULL, response_body=NULL
            WHERE id IN (
              SELECT id FROM request_logs
              WHERE created_at < ?
                AND (request_body IS NOT NULL OR response_body IS NOT NULL)
              LIMIT ?)
            """,
            (cutoff, BATCH),
        )
        n = cur.rowcount
        c.commit()
        total += n
        if n < BATCH:
            break
    return total


def delete_rows(c, cutoff):
    total = 0
    while True:
        cur = c.execute(
            """
            DELETE FROM request_logs
            WHERE id IN (SELECT id FROM request_logs WHERE created_at < ? LIMIT ?)
            """,
            (cutoff, BATCH),
        )
        n = cur.rowcount
        c.commit()
        total += n
        if n < BATCH:
            break
    return total


def main():
    c = sqlite3.connect(DB)
    before = c.execute("select count(*) from request_logs").fetchone()[0]
    print(f"rows before: {before}", flush=True)

    print("stage 1: null bodies older than 7d ...", flush=True)
    print(f"  affected: {null_bodies(c, days_ago(c, 7))}", flush=True)

    print("stage 2: delete rows older than 30d ...", flush=True)
    print(f"  affected: {delete_rows(c, days_ago(c, 30))}", flush=True)

    left = c.execute("select count(*) from request_logs").fetchone()[0]
    freelist = c.execute("PRAGMA freelist_count").fetchone()[0]
    pages = c.execute("PRAGMA page_count").fetchone()[0]
    print(f"rows after: {left}", flush=True)
    print(f"pages: {pages}  freelist: {freelist}", flush=True)
    c.close()


if __name__ == "__main__":
    sys.exit(main())
