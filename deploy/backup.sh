#!/usr/bin/env bash
# Nightly encrypted dump of the Career Compass database (lab server cron, 03:15).
# Only ciphertext is written: pg_dump is piped straight into age with the PUBLIC key in
# ~/cc-backups/recipient.txt. The private key (age identity) is never on this host; it lives
# in Hugo's password manager. The newest 14 dumps are kept. An off-site copy is pulled later.
# Restore: age -d -i <identity> cc-YYYYMMDD.sql.age | psql (the dump drops + recreates).
set -euo pipefail
cd "$(dirname "$0")"
OUT="$HOME/cc-backups"
AGE="$HOME/bin/age"
mkdir -p "$OUT" && chmod 700 "$OUT"
F="$OUT/cc-$(date +%Y%m%d).sql.age"
/usr/local/bin/docker-compose -p career-compass exec -T db \
  sh -c 'pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" --clean --if-exists' \
  | "$AGE" -R "$OUT/recipient.txt" > "$F.part"
mv "$F.part" "$F"
ls -1t "$OUT"/cc-*.sql.age | tail -n +15 | xargs -r rm --
echo "$(date -Is) ok $(basename "$F") $(stat -c %s "$F") B"
