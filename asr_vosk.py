#!/usr/bin/env python3
"""Vosk bridge for gabes-island.

Reads raw s16le 16 kHz mono from stdin (the sink monitor) in 1-second
buffers and prints `milliseconds<TAB>word` for every recognized word.
Nothing is played; the stream is only analyzed.
"""
import json
import sys

from vosk import KaldiRecognizer, Model, SetLogLevel

SetLogLevel(-1)

if len(sys.argv) < 2:
    sys.exit("usage: asr_vosk.py MODEL_DIR")

model = Model(sys.argv[1])
rec = KaldiRecognizer(model, 16000)
rec.SetWords(True)

CHUNK = 16000 * 2  # exactly 1 second of s16 mono
buffer_ms = 0


def emit(result, base_ms):
    for word in result.get("result", []):
        print(f"{base_ms + int(word['start'] * 1000)}\t{word['word']}", flush=True)


while True:
    data = sys.stdin.buffer.read(CHUNK)
    if not data:
        break
    # Process the 1 s buffer as one unit so the timeline stays exact.
    if rec.AcceptWaveform(data):
        emit(json.loads(rec.Result()), buffer_ms)
    else:
        emit(json.loads(rec.FinalResult()), buffer_ms)
    buffer_ms += int(len(data) / 2 / 16000 * 1000)
