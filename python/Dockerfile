# Build context is the repo root (see ../docker-compose.yml and
# ../ops/vps/docker-compose.yml: build.context: . / ../.., same convention
# crates/ws-server/Dockerfile uses) even though this service, unlike
# ws-server, has no workspace-wide deps to pull in -- kept consistent
# rather than special-cased so every service's compose block reads the
# same way.
FROM python:3.12-slim
WORKDIR /app

COPY python/requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY python/app ./app

# main.py's load_dotenv() call looks three directories up from itself for
# a repo-root .env -- that file is deliberately never baked into this
# image (same secrets-handling rule as crates/ws-server/Dockerfile: real
# credentials come from the container's own environment, via
# docker-compose's env_file:, not from anything COPYed in). dotenv no-ops
# silently when the file isn't found, so AlpacaConfig.from_env() just
# reads directly from the environment compose already injected.
ENV PYTHONUNBUFFERED=1
EXPOSE 8000
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
