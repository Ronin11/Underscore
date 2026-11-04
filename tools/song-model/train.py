#!/usr/bin/env python3
"""Fit crates/library/src/song_model.rs: the phone's measurements of a song
to the energy and valence CLAP gives it on a GPU.

    underscore songs features <music folder> > features.jsonl
    train.py features.jsonl labels.json [--write crates/library/src/song_model.rs]

labels.json is a list of songs with path, artist, energy_raw and valence_raw,
the differences of CLAP similarities behind worker/underscore_worker/music.py
Index.axes(). A file and a label are matched by file name without its track
number. Numbers are always reported on music the fit did not see: whole albums
held out, and everything outside the biggest collection held out. Pass
--cues <map.toml> to judge song picks on a real map's cues. Needs numpy.
"""
import argparse, collections, json, os, re
import numpy as np

# Kept in step with worker/underscore_worker/music.py.
ENERGY0, ENERGY1, VALENCE0, VALENCE1 = 0.065, 0.096, 0.103, 0.150
L2S = (30, 100, 300, 1000, 3000)
GRID = [(e, v) for e in np.arange(1.5, 5, 0.5) for v in np.arange(-0.75, 1, 0.25)]


def stem(path):
    return re.sub(r"^[\d\s._-]+", "", os.path.splitext(os.path.basename(path))[0]).lower()


def load(features, labels):
    by_stem = collections.defaultdict(list)
    for s in json.load(open(labels)):
        by_stem[stem(s["path"])].append(s)
    paths, x, e, v, groups = [], [], [], [], []
    missed = 0
    for line in open(features):
        row = json.loads(line)
        found = by_stem.get(stem(row["path"]))
        if not found:
            missed += 1
            continue
        s = next((c for c in found if c["path"].split("/")[0].lower() in row["path"].lower()), found[0])
        paths.append(row["path"])
        x.append(row["features"])
        e.append(np.clip(3 + (s["energy_raw"] - ENERGY0) / ENERGY1, 1, 5))
        v.append(np.clip((s["valence_raw"] - VALENCE0) / (2 * VALENCE1), -1, 1))
        groups.append(os.path.dirname(row["path"]))
    print(f"{len(paths)} songs matched, {missed} without a label, {len(set(groups))} albums")
    return paths, np.array(x, float), np.array(e), np.array(v), groups


def fit(x, y, l2):
    mu, sd = x.mean(0), x.std(0) + 1e-9
    z = (x - mu) / sd
    m = y.mean()
    w = np.linalg.solve(z.T @ z + l2 * np.eye(z.shape[1]), z.T @ (y - m))
    return mu, sd, m, w


def predict(model, x):
    mu, sd, m, w = model
    return (x - mu) / sd @ w + m


def judge(name, pe, pv, e, v, cues):
    def dist(a, b, c):
        return ((a - c[0]) / 2) ** 2 + (b - c[1]) ** 2
    rank, extra, chance, overlap, picks = [], [], [], [], []
    for c in cues:
        truth, guess = dist(e, v, c), dist(pe, pv, c)
        i = int(np.argmin(guess))
        picks.append(i)
        rank.append((truth < truth[i]).mean())
        extra.append(truth[i] - truth.min())
        chance.append(truth.mean() - truth.min())
        overlap.append(len(set(np.argsort(guess)[:10]) & set(np.argsort(truth)[:10])) / 10)
    r = np.corrcoef(pe, e)[0, 1], np.corrcoef(pv, v)[0, 1]
    print(f"{name:32s} n={len(e):4d}  r {r[0]:.2f}/{r[1]:.2f}  error {abs(pe - e).mean():.2f}/{abs(pv - v).mean():.2f}"
          f"  pick rank {np.mean(rank) * 100:4.1f}%  excess {np.mean(extra):.3f} (chance {np.mean(chance):.3f})"
          f"  top-10 shared {np.mean(overlap) * 100:3.0f}%  {len(set(picks))} songs")
    return np.mean(rank)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("features")
    ap.add_argument("labels")
    ap.add_argument("--write")
    ap.add_argument("--l2", type=float)
    ap.add_argument("--cues")
    ap.add_argument("--version", type=int, default=1)
    a = ap.parse_args()
    paths, x, e, v, groups = load(a.features, a.labels)
    cues = GRID
    if a.cues:
        cues = [(float(p), float(q)) for p, q in re.findall(r"energy = ([\d.-]+)\nvalence = ([\d.-]+)", open(a.cues).read())]
    print(f"{len(cues)} cues; library energy {e.mean():.2f} +/- {e.std():.2f}, valence {v.mean():.2f} +/- {v.std():.2f}")

    albums = sorted(set(groups))
    np.random.default_rng(0).shuffle(albums)
    fold = {g: i % 5 for i, g in enumerate(albums)}
    f = np.array([fold[g] for g in groups])
    tops = np.array([re.sub(r".*/Music/", "", p).split("/")[0] for p in paths])
    big = collections.Counter(tops).most_common(1)[0][0]
    outside = tops != big
    best = (9.0, None)
    for l2 in L2S:
        pe, pv = np.zeros(len(e)), np.zeros(len(e))
        for k in range(5):
            tr, te = f != k, f == k
            pe[te] = predict(fit(x[tr], e[tr], l2), x[te])
            pv[te] = predict(fit(x[tr], v[tr], l2), x[te])
        rank = judge(f"albums held out, l2={l2}", pe, pv, e, v, cues)
        if rank < best[0]:
            best = (rank, l2)
        if outside.any():
            judge(f"  only {big} seen", predict(fit(x[~outside], e[~outside], l2), x[outside]),
                  predict(fit(x[~outside], v[~outside], l2), x[outside]), e[outside], v[outside], cues)
    l2 = a.l2 or best[1]
    print(f"l2 = {l2}")
    if not a.write:
        return
    me, ve = fit(x, e, l2), fit(x, v, l2)

    def array(name, values):
        body = ", ".join(f"{t:.6e}" for t in values)
        return f"pub const {name}: [f32; {len(values)}] = [{body}];\n"
    with open(a.write, "w") as out:
        out.write("//! Written by tools/song-model/train.py. Do not edit by hand.\n")
        out.write("//! Bump VERSION with any change: scores from another model are thrown away.\n\n")
        out.write("#![allow(clippy::unreadable_literal, clippy::excessive_precision)]\n\n")
        out.write(f"pub const VERSION: u32 = {a.version};\n")
        out.write(f"pub const ENERGY_MEAN: f32 = {me[2]:.6};\npub const VALENCE_MEAN: f32 = {ve[2]:.6};\n")
        out.write(array("MEAN", me[0]) + array("SPREAD", me[1]) + array("ENERGY", me[3]) + array("VALENCE", ve[3]))
    print(f"wrote {a.write}")


if __name__ == "__main__":
    main()
