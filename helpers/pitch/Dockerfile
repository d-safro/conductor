# Pitch ("What's playing?"): Python with numpy and ffmpeg, and song_id.py. Prelude builds it on your computer.
FROM python:3.13-slim
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg && rm -rf /var/lib/apt/lists/* \
 && pip install --no-cache-dir "numpy==2.4.6"
WORKDIR /app
COPY song_id.py /app/song_id.py
CMD ["python", "-u", "/app/song_id.py"]
