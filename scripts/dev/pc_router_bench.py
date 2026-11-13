"""EXPERIMENT (exp/pc-actions). Offline bench of the PC-command router; needs
sentence-transformers, scikit-learn and (for the Laya rows) the laya SDK.
Run: python scripts/dev/pc_router_bench.py big
"""
"""PC-command router bench: e5 kNN / +none / +logreg / e5-base / Laya rich / hybrid.

Train = the router's EXAMPLES (parsed from pc_router.rs) + NONE_EXAMPLES below.
Test  = pc_testset.jsonl (written independently). Metrics per method:
  acc        intent accuracy over all lines (none included)
  sure_wrong lines where the method is "sure" (score >= its threshold) and wrong
  none_fa    out-of-scope lines it would execute while sure
  coverage   share of in-scope lines it is sure about AND right
  ms         median latency per command
Thresholds are picked per method on a held-out half (dev) so that sure_wrong
is minimal, then reported on the other half (test). Seeded split.
"""
import json, os, random, re, statistics, sys, time

sys.stdout.reconfigure(encoding="utf-8")
import numpy as np  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
MAP = {"Open": "open", "Tile": "tile", "Move": "move", "FindFiles": "find_files", "Minimize": "minimize",
       "Web": "web", "Browser": "browser", "Media": "media", "Folder": "folder", "Settings": "settings"}

ASSET = json.load(open(os.path.join(HERE, "..", "..", "core", "assets", "pc-router-examples.json"), encoding="utf-8"))
TRAIN = [(k, t) for k, v in ASSET.items() if k not in ("_comment", "none") for t in v]

# Written for this bench, NOT taken from the test set: dictation, questions,
# unsupported requests, fragments. They teach "this is not a command".
NONE_EXAMPLES = [
    "ciao Paolo, ci vediamo domani in ufficio", "ti mando il preventivo entro stasera",
    "la riunione è stata spostata a giovedì", "grazie mille per il supporto",
    "ho finito di scrivere il report", "che tempo fa oggi", "quanto fa dodici per otto",
    "manda una mail al cliente", "chiama la mamma", "spegni il computer", "cancella questo file",
    "elimina la cartella", "compra le scarpe su amazon", "prenota un tavolo per due",
    "ok", "va bene", "aspetta", "no niente", "apri la", "metti", "allora", "boh non so",
    "thanks for your help", "let's meet tomorrow at ten", "send the invoice to John",
    "what time is it", "delete this", "shut down the pc", "okay", "never mind",
    "il progetto è in ritardo di due settimane", "domani porto io il caffè",
]
TRAIN_NONE = TRAIN + [("none", t) for t in ASSET["none"]]
EXTRA_PATH = os.path.join(HERE, "pc_train_extra.json")
EXTRA = []
if False:  # the asset already holds the extra examples
    for k, v in json.load(open(EXTRA_PATH, encoding="utf-8")).items():
        EXTRA += [(k, t) for t in v]
TRAIN_BIG = TRAIN_NONE + EXTRA
TEST = [json.loads(l) for l in open(os.path.join(HERE, "..", "..", "core", "tests", "fixtures", "pc_testset.jsonl"), encoding="utf-8") if l.strip()]
INTENTS = sorted({i for i, _ in TRAIN}) + ["none"]

overlap = {t.lower() for _, t in TRAIN} & {r["text"].lower() for r in TEST}
print(f"train {len(TRAIN)} (+{len(NONE_EXAMPLES)} none), test {len(TEST)}, exact overlap {len(overlap)}")

rnd = random.Random(7)
idx = list(range(len(TEST)))
rnd.shuffle(idx)
DEV, EVAL = idx[: len(idx) // 2], idx[len(idx) // 2:]


def evaluate(name, preds):
    """preds[i] = (intent, score, ms). Picks the threshold on DEV, reports on EVAL."""
    def stats(ids, thr):
        right = sum(preds[i][0] == TEST[i]["intent"] for i in ids)
        sure = [i for i in ids if preds[i][1] >= thr and preds[i][0] != "none"]
        sure_wrong = sum(preds[i][0] != TEST[i]["intent"] for i in sure)
        none_fa = sum(TEST[i]["intent"] == "none" for i in sure)
        in_scope = [i for i in ids if TEST[i]["intent"] != "none"]
        cov = sum(1 for i in sure if preds[i][0] == TEST[i]["intent"]) / max(1, len(in_scope))
        return right / len(ids), sure_wrong, none_fa, cov
    scores = sorted({round(p[1], 4) for p in preds})
    best = None
    for thr in scores:
        acc, sw, fa, cov = stats(DEV, thr)
        key = (sw, -cov)  # fewest confident mistakes, then most coverage
        if best is None or key < best[0]:
            best = (key, thr)
    thr = best[1]
    acc, sw, fa, cov = stats(EVAL, thr)
    ms = statistics.median(p[2] for p in preds)
    print(f"{name:34s} acc {acc:5.1%}  thr {thr:7.4f}  sure_wrong {sw:2d}  none_fa {fa:2d}  coverage {cov:5.1%}  {ms:6.1f} ms")
    return thr


def knn(model_name, prefix, train, scorer="margin"):
    from sentence_transformers import SentenceTransformer
    m = SentenceTransformer(model_name, device="cpu")
    labels = [i for i, _ in train]
    E = m.encode([prefix + t for _, t in train], normalize_embeddings=True)
    m.encode([prefix + "warm"], normalize_embeddings=True)
    preds = []
    for r in TEST:
        t = time.perf_counter()
        q = m.encode([prefix + r["text"]], normalize_embeddings=True)[0]
        sims = E @ q
        best = {}
        for lab, s in zip(labels, sims):
            best[lab] = max(best.get(lab, -1.0), float(s))
        ranked = sorted(best.items(), key=lambda kv: -kv[1])
        score = ranked[0][1] - ranked[1][1] if scorer == "margin" else ranked[0][1]
        preds.append((ranked[0][0], score, (time.perf_counter() - t) * 1000, ranked[:3]))
    return m, E, labels, preds


def logreg(m, prefix, train):
    from sklearn.linear_model import LogisticRegression
    X = m.encode([prefix + t for _, t in train], normalize_embeddings=True)
    y = [i for i, _ in train]
    clf = LogisticRegression(max_iter=2000, C=8.0).fit(X, y)
    preds = []
    for r in TEST:
        t = time.perf_counter()
        q = m.encode([prefix + r["text"]], normalize_embeddings=True)
        p = clf.predict_proba(q)[0]
        k = int(np.argmax(p))
        preds.append((clf.classes_[k], float(p[k]), (time.perf_counter() - t) * 1000))
    return preds


DESC = {
    "open": "aprire un'app o portarla in primo piano (apri Teams, mi apri Excel, lancia Spotify, open Outlook)",
    "tile": "mettere due finestre affiancate (affianca Word e Chrome, dividi lo schermo, side by side)",
    "move": "spostare o ingrandire una finestra: sinistra, destra, schermo intero, altro monitor (sposta Chrome a destra, massimizza Word)",
    "minimize": "ridurre a icona o nascondere una finestra (nascondi Outlook, riduci a icona, minimize Slack)",
    "find_files": "cercare file o documenti sul computer (trova i file di ieri, dov'è il preventivo)",
    "web": "aprire un sito o cercare su Google, YouTube, Amazon, Wikipedia (cerca su YouTube, vai su Amazon)",
    "browser": "comando del browser: nuova scheda, chiudi scheda, indietro, avanti, ricarica",
    "media": "musica e volume: pausa, play, brano successivo, alza o abbassa il volume, muto",
    "folder": "aprire una cartella: Download, Documenti, Desktop, Immagini, ultimo file scaricato",
    "settings": "aprire le impostazioni di Windows: Bluetooth, Wi-Fi, audio, schermo, notifiche",
    "none": "non è un comando per il computer: testo dettato, una domanda, una richiesta non supportata (mail, spegni, cancella) o una frase a metà",
}


def laya_preds(candidates_of=None):
    import laya
    agent = laya.load("convaiinnovations/laya-multilingual")
    agent.predict({"command": "warm"}, {"q": {"type": "noul", "instructions": "x"}})
    preds = []
    for n, r in enumerate(TEST):
        opts = DESC if candidates_of is None else {k: DESC[k] for k in candidates_of[n]}
        q = {"intent": {"type": "choice",
                        "instructions": "A person speaks to their Windows PC. Which action does `command` ask for?",
                        "criteria": opts}}
        t = time.perf_counter()
        a = agent.predict({"command": r["text"]}, q)["answers"]["intent"]
        ms = (time.perf_counter() - t) * 1000
        probs = a.get("probabilities") or {}
        top = sorted(probs.values(), reverse=True) + [0, 0]
        preds.append((a["choice"], top[0] - top[1], ms))
    return preds


def centroid(m, prefix, train):
    labels = sorted({i for i, _ in train})
    X = m.encode([prefix + t for _, t in train], normalize_embeddings=True)
    C = []
    for lab in labels:
        v = X[[i for i, (l, _) in enumerate(train) if l == lab]].mean(axis=0)
        C.append(v / np.linalg.norm(v))
    C = np.stack(C)
    preds = []
    for r in TEST:
        t = time.perf_counter()
        q = m.encode([prefix + r["text"]], normalize_embeddings=True)[0]
        sims = C @ q
        o = np.argsort(-sims)
        preds.append((labels[o[0]], float(sims[o[0]] - sims[o[1]]), (time.perf_counter() - t) * 1000))
    return preds


def big():
    print(f"BIG train: {len(TRAIN_BIG)} examples ({len(EXTRA)} extra)")
    for name in ["intfloat/multilingual-e5-small", "intfloat/multilingual-e5-base"]:
        tag = name.split("-")[-1]
        m, _, _, pk = knn(name, "query: ", TRAIN_BIG)
        evaluate(f"{tag} kNN big", [x[:3] for x in pk])
        evaluate(f"{tag} centroid big", centroid(m, "query: ", TRAIN_BIG))
        evaluate(f"{tag} logreg big", logreg(m, "query: ", TRAIN_BIG))


if __name__ == "__main__" and sys.argv[1:] == ["big"]:
    big()
    sys.exit(0)

if __name__ == "__main__":
    which = sys.argv[1:] or ["e5", "e5none", "logreg", "e5base", "laya", "hybrid"]
    e5 = None
    if {"e5", "e5none", "logreg", "hybrid"} & set(which):
        e5, E, labels, p1 = knn("intfloat/multilingual-e5-small", "query: ", TRAIN)
        if "e5" in which:
            evaluate("1 e5-small kNN (today)", [x[:3] for x in p1])
        m2, _, _, p2 = knn("intfloat/multilingual-e5-small", "query: ", TRAIN_NONE)
        if "e5none" in which:
            evaluate("2 e5-small kNN + none class", [x[:3] for x in p2])
        if "logreg" in which:
            evaluate("3 e5-small logreg + none", logreg(m2, "query: ", TRAIN_NONE))
        if "hybrid" in which:
            cands = [[k for k, _ in x[3]] + ["none"] for x in p2]
            cands = [list(dict.fromkeys(c)) for c in cands]
            hp = laya_preds(cands)
            hp = [(h[0], h[1], h[2] + p2[i][2]) for i, h in enumerate(hp)]
            evaluate("6 hybrid e5 top3 -> Laya", hp)
    if "e5base" in which:
        _, _, _, p4 = knn("intfloat/multilingual-e5-base", "query: ", TRAIN_NONE)
        evaluate("4 e5-base kNN + none class", [x[:3] for x in p4])
    if "laya" in which:
        evaluate("5 Laya rich context (all intents)", laya_preds())
