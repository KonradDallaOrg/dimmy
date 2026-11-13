"""EXPERIMENT (exp/pc-actions). Which embedding model routes PC commands best?

Same pipeline as the shipped router (embed -> softmax classifier trained on
core/assets/pc-router-examples.json), only the encoder changes. Test set:
core/tests/fixtures/pc_testset.jsonl. Reports accuracy, the live operating
point (no wrong action), size and CPU latency per command.
Run: python scripts/dev/pc_router_models.py [model ids...]
"""
import json, os, statistics, sys, time

sys.stdout.reconfigure(encoding="utf-8")
import numpy as np  # noqa: E402
from sklearn.linear_model import LogisticRegression  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..", "..")
ASSET = json.load(open(os.path.join(ROOT, "core", "assets", "pc-router-examples.json"), encoding="utf-8"))
TRAIN = [(k, t) for k, v in ASSET.items() if k != "_comment" for t in v]
TEST = [json.loads(l) for l in open(os.path.join(ROOT, "core", "tests", "fixtures", "pc_testset.jsonl"), encoding="utf-8") if l.strip()]
seen = {t.lower() for _, t in TRAIN}
TEST = [r for r in TEST if r["text"].lower() not in seen]

# (id, query prefix, extra kwargs)
CANDIDATES = [
    ("intfloat/multilingual-e5-small", "query: ", {}),
    ("intfloat/multilingual-e5-base", "query: ", {}),
    ("sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2", "", {}),
    ("sentence-transformers/LaBSE", "", {}),
    ("BAAI/bge-m3", "", {}),
    ("Alibaba-NLP/gte-multilingual-base", "", {"trust_remote_code": True}),
    ("google/embeddinggemma-300m", "task: classification | query: ", {}),
    ("minishlab/potion-multilingual-128M", "", {}),
]


def run(model_id, prefix, kw):
    from sentence_transformers import SentenceTransformer
    t0 = time.perf_counter()
    m = SentenceTransformer(model_id, device="cpu", **kw)
    load_s = time.perf_counter() - t0
    params = sum(p.numel() for p in m.parameters()) / 1e6 if hasattr(m, "parameters") else float("nan")
    enc = lambda xs: m.encode([prefix + x for x in xs], normalize_embeddings=True)
    clf = LogisticRegression(max_iter=3000, C=8.0).fit(enc([t for _, t in TRAIN]), [k for k, _ in TRAIN])
    enc(["warm up"])
    preds, ms = [], []
    for r in TEST:
        t = time.perf_counter()
        p = clf.predict_proba(enc([r["text"]]))[0]
        ms.append((time.perf_counter() - t) * 1000)
        k = int(np.argmax(p))
        preds.append((clf.classes_[k], float(p[k])))
    gold = [r["intent"] for r in TEST]
    acc = np.mean([p[0] == g for p, g in zip(preds, gold)])
    inscope = sum(g != "none" for g in gold)
    # live operating point: the lowest threshold with no wrong action executed
    best = (1.0, 0.0)
    for thr in np.arange(0.30, 0.96, 0.01):
        sure = [(p, g) for p, g in zip(preds, gold) if p[1] >= thr and p[0] != "none"]
        if all(p[0] == g for p, g in sure):
            cov = sum(p[0] == g for p, g in sure) / inscope
            best = (thr, cov)
            break
    print(f"{model_id:60s} {params:6.0f}M  acc {acc:5.1%}  live thr {best[0]:.2f} -> {best[1]:5.1%} decided  "
          f"{statistics.median(ms):6.1f} ms/cmd  load {load_s:4.1f}s", flush=True)


if __name__ == "__main__":
    wanted = sys.argv[1:]
    print(f"train {len(TRAIN)}  test {len(TEST)}")
    for mid, pre, kw in CANDIDATES:
        if wanted and mid not in wanted:
            continue
        try:
            run(mid, pre, kw)
        except Exception as e:  # a gated or broken model must not stop the others
            print(f"{mid:60s} SKIPPED: {type(e).__name__}: {str(e)[:120]}", flush=True)
