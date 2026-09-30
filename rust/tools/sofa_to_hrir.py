"""Convert SOFA HRTF files to the tiny `.hrir` format the dsp front/back trainer reads.

    pip install h5py numpy scipy
    python tools/sofa_to_hrir.py <dir with *.sofa> <out dir>

Keeps the horizontal plane (|elevation| <= 10 deg), resamples to 48 kHz, and writes per file:
u32 count, u32 taps, then per direction: f32 azimuth (deg, 0 front, + right), taps x f32 left,
taps x f32 right (all little-endian).

Sources used so far (from https://sofacoustics.org/data/database/): MIT KEMAR normal pinna and
CIPIC subjects. Check each database's licence/citation terms; only derived classifier weights
are committed, never the HRIR data.
"""
import glob, os, struct, sys
from math import gcd
import h5py, numpy as np
from scipy.signal import resample_poly

SR = 48000
src, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
for path in sorted(glob.glob(os.path.join(src, "*.sofa"))):
    with h5py.File(path, "r") as f:
        ir = f["Data.IR"][:]                      # (M, 2, N): receiver 0 = left, 1 = right
        pos = f["SourcePosition"][:]              # SOFA azimuth: counter-clockwise from front
        fs = int(round(float(np.ravel(f["Data.SamplingRate"][:])[0])))
    keep = np.abs(pos[:, 1]) <= 10
    ir, az = ir[keep], (-pos[keep, 0] + 180) % 360 - 180
    if fs != SR:
        g = gcd(fs, SR)
        ir = resample_poly(ir, SR // g, fs // g, axis=2)
    ir = ir.astype("<f4")
    name = os.path.join(out, os.path.basename(path).replace(".sofa", ".hrir"))
    with open(name, "wb") as o:
        o.write(struct.pack("<II", len(az), ir.shape[2]))
        for a, h in zip(az, ir):
            o.write(struct.pack("<f", a))
            o.write(h[0].tobytes())
            o.write(h[1].tobytes())
    print(f"{name}: {len(az)} directions, {ir.shape[2]} taps")
