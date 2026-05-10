FROM python:3.11-slim

WORKDIR /app

COPY docker/sidecars/bonsai-native-parser/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir -r /app/requirements.txt

COPY docker/sidecars/bonsai-native-parser/app.py /app/app.py

EXPOSE 9102

CMD ["python", "/app/app.py"]
