#!/usr/bin/env python3
import math
import sys
import wave


def read_mono_i16(path):
    with wave.open(path, "rb") as recording:
        channels = recording.getnchannels()
        rate = recording.getframerate()
        width = recording.getsampwidth()
        frames = recording.getnframes()
        data = recording.readframes(frames)
    if width != 2:
        raise SystemExit(f"expected 16-bit PCM WAV, got sample width {width}")
    if channels != 1:
        raise SystemExit(f"expected mono WAV, got {channels} channels")
    samples = [
        int.from_bytes(data[index:index + 2], "little", signed=True)
        for index in range(0, len(data), 2)
    ]
    return rate, samples


def loudest_window(samples, rate):
    window_size = min(rate, len(samples))
    if window_size == 0:
        raise SystemExit("recording contains no samples")
    step = max(1, window_size // 10)
    best_start = 0
    best_energy = -1
    for start in range(0, max(1, len(samples) - window_size + 1), step):
        window = samples[start:start + window_size]
        energy = sum(sample * sample for sample in window)
        if energy > best_energy:
            best_start = start
            best_energy = energy
    return samples[best_start:best_start + window_size]


def goertzel_power(samples, rate, frequency):
    coefficient = 2 * math.cos(2 * math.pi * frequency / rate)
    q0 = q1 = q2 = 0.0
    for sample in samples:
        q0 = coefficient * q1 - q2 + sample
        q2 = q1
        q1 = q0
    return q1 * q1 + q2 * q2 - coefficient * q1 * q2


def frequency_peaks(samples, rate):
    window = loudest_window(samples, rate)
    mean = sum(window) / len(window)
    window = [sample - mean for sample in window]
    powers = [
        (frequency, goertzel_power(window, rate, frequency))
        for frequency in range(100, min(8_000, rate // 2), 10)
    ]
    strongest = max(power for _, power in powers)
    peaks = [
        (frequency, power)
        for index, (frequency, power) in enumerate(powers)
        if power >= strongest * 0.005
        and (index == 0 or power >= powers[index - 1][1])
        and (index == len(powers) - 1 or power >= powers[index + 1][1])
    ]
    peaks.sort(key=lambda peak: peak[1], reverse=True)
    return [frequency for frequency, _ in peaks[:12]]


def dominant_frequency(samples, rate):
    return frequency_peaks(samples, rate)[0]


def main():
    if len(sys.argv) != 4:
        raise SystemExit("usage: analyze-recording.py FIXTURE RECORDING PLAYBACK_RATE")
    fixture_path, recording_path, playback_rate = sys.argv[1], sys.argv[2], float(sys.argv[3])
    fixture_rate, fixture = read_mono_i16(fixture_path)
    recording_rate, recording = read_mono_i16(recording_path)
    if recording_rate != 48_000:
        raise SystemExit(f"expected 48000 Hz recording, got {recording_rate} Hz")

    peak = max(abs(sample) for sample in recording)
    rms = math.sqrt(sum(sample * sample for sample in recording) / len(recording))
    if peak <= 1_000 or rms <= 100:
        raise SystemExit(f"recording is silent or too quiet: peak={peak} rms={rms}")

    expected_tones = frequency_peaks(fixture, fixture_rate)
    if playback_rate != 1.0:
        expected_tones += [tone * playback_rate for tone in expected_tones]
    observed = dominant_frequency(recording, recording_rate)
    matched = min(expected_tones, key=lambda tone: abs(observed - tone))
    tolerance = max(120.0, matched * 0.08)
    if abs(observed - matched) > tolerance:
        expected = " or ".join(f"{tone:.0f}Hz" for tone in expected_tones)
        raise SystemExit(
            f"recording does not match beep tone: expected≈{expected} observed≈{observed:.0f}Hz peak={peak} rms={rms:.0f}"
        )
    print(
        f"beep detected: expected≈{matched:.0f}Hz observed≈{observed:.0f}Hz peak={peak} rms={rms:.0f}"
    )


if __name__ == "__main__":
    main()
