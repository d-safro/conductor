# Metronome (Automix analysis): Python with numpy and ffmpeg, and mix_analyzer.py. Prelude builds it on your computer.
FROM python:3.13-slim
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg && rm -rf /var/lib/apt/lists/* \
 && pip install --no-cache-dir "numpy==2.4.6"
WORKDIR /app
COPY mix_analyzer.py /app/mix_analyzer.py
CMD ["python", "-u", "/app/mix_analyzer.py"]
