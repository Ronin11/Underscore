"""Reading stretches of audio for analysis: mono floats at the rate a model
wants. Only for listening to the files; nothing here writes audio."""
from pathlib import Path

import av
import numpy as np


def read(path: Path, start: float, end: float | None, rate: int) -> np.ndarray:
    """Mono f32 samples of `path` from `start` to `end` seconds (None for
    the end of the file) at `rate`."""
    with av.open(str(path)) as container:
        stream = container.streams.audio[0]
        resampler = av.AudioResampler(format="flt", layout="mono", rate=rate)
        if start > 0:
            container.seek(int(start * av.time_base), any_frame=False, backward=True)
        chunks, first = [], None
        for frame in container.decode(stream):
            if frame.time is None:
                continue
            if first is None:
                first = frame.time
            if end is not None and frame.time >= end:
                break
            for out in resampler.resample(frame):
                chunks.append(out.to_ndarray().reshape(-1))
        for out in resampler.resample(None):
            chunks.append(out.to_ndarray().reshape(-1))
    if not chunks:
        return np.zeros(0, dtype=np.float32)
    samples = np.concatenate(chunks).astype(np.float32, copy=False)
    # A seek lands on the frame before; trim to the asked start.
    skip = int(max(0.0, start - (first or 0.0)) * rate)
    samples = samples[skip:]
    if end is not None:
        samples = samples[: int((end - start) * rate)]
    return samples


def length(path: Path) -> float:
    with av.open(str(path)) as container:
        if container.duration:
            return container.duration / av.time_base
        stream = container.streams.audio[0]
        return float(stream.duration * stream.time_base) if stream.duration else 0.0
