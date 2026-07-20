#!/bin/sh
# Entrypoint for the RemembrallMCP Docker image.
#
# `remembrall init` is a one-shot setup command: it creates the schema and
# downloads the embedding model, then exits. If it were the container's only
# command, the container would stop immediately and `docker compose exec ...`
# would fail with "service is not running".
#
# This script runs init on every start (it is idempotent: the schema uses
# CREATE ... IF NOT EXISTS, the model is cached in the persisted volume, and
# the config file is re-saved) and then hands off to the MCP server, which
# stays alive and listens on stdio.
#
# init's output is redirected to stderr so stdout stays clean for MCP
# JSON-RPC when an MCP client launches the container via
# `docker compose run --rm -T remembrall`.
set -e

remembrall init --database-url "$DATABASE_URL" >&2

# Run the provided command. With no args this is `remembrall`, which defaults
# to `serve` (the MCP server over stdio).
exec remembrall "$@"