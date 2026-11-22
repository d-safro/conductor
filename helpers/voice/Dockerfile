# Interlude's voice: Piper (offline text-to-speech) and dj_voice.py. Prelude builds it on your computer.
FROM python:3.13-slim
RUN pip install --no-cache-dir "piper-tts==1.3.0"
WORKDIR /app
COPY dj_voice.py /app/dj_voice.py
CMD ["python", "-u", "/app/dj_voice.py"]
