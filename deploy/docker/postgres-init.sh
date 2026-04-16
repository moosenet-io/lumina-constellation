#!/bin/bash
# Create additional databases for Lumina services
set -e

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-EOSQL
  CREATE DATABASE IF NOT EXISTS litellm;
  CREATE DATABASE IF NOT EXISTS lumina_inbox;
  CREATE DATABASE IF NOT EXISTS ironclaw;

  -- LiteLLM user
  DO \$\$
  BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'litellm_user') THEN
      CREATE USER litellm_user WITH PASSWORD '${LITELLM_DB_PASS:-litellm-dev-pass}';
    END IF;
  END
  \$\$;
  GRANT ALL PRIVILEGES ON DATABASE litellm TO litellm_user;

  -- Inbox user
  DO \$\$
  BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'lumina_inbox_user') THEN
      CREATE USER lumina_inbox_user WITH PASSWORD '${INBOX_DB_PASS:-inbox-dev-pass}';
    END IF;
  END
  \$\$;
  GRANT ALL PRIVILEGES ON DATABASE lumina_inbox TO lumina_inbox_user;
EOSQL
