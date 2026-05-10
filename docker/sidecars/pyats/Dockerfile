FROM python:3.11-slim

WORKDIR /app

COPY docker/sidecars/pyats/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir -r /app/requirements.txt

COPY docker/sidecars/pyats/app.py /app/app.py

EXPOSE 9101

CMD ["python", "/app/app.py"]
