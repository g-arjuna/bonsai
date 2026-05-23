FROM python:3.11-slim

WORKDIR /app

# System deps for pyats SSH + crypto
RUN apt-get update && apt-get install -y --no-install-recommends \
    openssh-client gcc libffi-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Python deps (pyats + genie are heavy — cached in a separate layer)
COPY docker/sidecars/pyats/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir -r /app/requirements.txt

COPY docker/sidecars/pyats/app.py /app/app.py

EXPOSE 9101

HEALTHCHECK --interval=30s --timeout=5s CMD python -c "import urllib.request; urllib.request.urlopen('http://localhost:9101/healthz')"

CMD ["python", "/app/app.py"]
