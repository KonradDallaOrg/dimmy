"""Quick audit of graphify graph.json to surface signal-vs-noise.

Reports:
  - Node count per top-level directory (where mass lives)
  - Generated build-artifact noise (obj/, bin/, .g.i.cs)
  - Orphan nodes (degree 0 — dead code candidates)
  - Top god nodes (highest degree)
  - Duplicate label nodes (same name at multiple file paths)
"""
import json, sys
from collections import Counter, defaultdict

g = json.load(open('graphify-out/graph.json'))
nodes = g.get('nodes', [])
edges = g.get('edges', [])
print(f"nodes={len(nodes)} edges={len(edges)}")

edges = g.get('edges') or g.get('links') or []
# Edges may be a dict-of-lists keyed by node id, or flat list of {source,target}
deg = Counter()
if isinstance(edges, dict):
    for src, targets in edges.items():
        for t in targets:
            tid = t if isinstance(t, str) else (t.get('target') or t.get('id'))
            deg[src] += 1
            if tid: deg[tid] += 1
else:
    for e in edges:
        s = e.get('source') or e.get('src')
        t = e.get('target') or e.get('dst')
        if s: deg[s] += 1
        if t: deg[t] += 1
print(f"edges resolved: {sum(deg.values())//2 if deg else 0} (deg-pair count)")

# Bucket by top-level dir
buckets = Counter()
generated_count = 0
generated_samples = []
for n in nodes:
    src = (n.get('source_file') or n.get('source_path') or '').replace('\\', '/')
    if 'obj/' in src or '/bin/' in src or '.g.i.cs' in src or '.g.cs' in src:
        generated_count += 1
        if len(generated_samples) < 5:
            generated_samples.append((n.get('label', '?'), src))
    if src:
        parts = src.split('/')
        buckets[parts[0]] += 1

print("\nTop-level dir node counts:")
for d, c in buckets.most_common(10):
    print(f"  {c:6d}  {d}")

print(f"\nGenerated build-artifact nodes: {generated_count}")
for lbl, src in generated_samples:
    print(f"  {lbl}  <-  {src}")

# Orphans (zero edges)
orphans = [n for n in nodes if deg.get(n.get('id'), 0) == 0]
print(f"\nOrphan nodes (degree=0): {len(orphans)}")
# Surface orphans with file paths (skip ones from generated dirs)
def src_of(n): return (n.get('source_file') or n.get('source_path') or '').replace('\\','/')
real_orphans = [(n.get('label','?'), src_of(n))
                for n in orphans
                if 'obj/' not in src_of(n)
                and '/bin/' not in src_of(n)
                and '.g.i.cs' not in src_of(n)
                and 'graphify-out/' not in src_of(n)
                and n.get('label','').lower() not in {'', 'unknown'}]
print(f"  Real-code orphans (after filtering generated): {len(real_orphans)}")
for lbl, src in real_orphans[:15]:
    print(f"    {lbl}  <-  {src}")

# Duplicate labels (same name, multiple paths) — often cross-platform parity OR rename leftovers
by_label = defaultdict(list)
for n in nodes:
    lbl = n.get('label')
    src = src_of(n)
    if not lbl or 'obj/' in src or '/bin/' in src or '.g.i.cs' in src or 'graphify-out/' in src:
        continue
    by_label[lbl].append(src)
dupes = [(l, paths) for l, paths in by_label.items() if len(paths) >= 2]
dupes.sort(key=lambda x: -len(x[1]))
print(f"\nDuplicate labels (same name, multiple source files): {len(dupes)}")
# Filter to interesting ones — function/method-like names
interesting = [(l, p) for l, p in dupes
               if l and l[0].islower() or '_' in l or l.endswith('()')]
print(f"  Function-like duplicates: {len(interesting)}")
for lbl, paths in interesting[:12]:
    print(f"    {lbl}  ({len(paths)}x)")
    for p in paths[:3]:
        print(f"      - {p}")

# Top god nodes — skip type/decorator names that aren't real symbols.
# Tree-sitter emits one node per occurrence of every type identifier
# (`c_int`, `string`, `DllImport`, …) which inflates the top-N with
# things you can't refactor. Blocklist keeps the chart actionable.
TYPE_NOISE = {
    # Rust FFI primitives + std types
    'c_int', 'c_char', 'c_void', 'c_uint', 'c_long', 'c_ulong',
    'c_short', 'c_ushort', 'c_float', 'c_double', 'c_schar', 'c_uchar',
    'u8', 'u16', 'u32', 'u64', 'usize', 'i8', 'i16', 'i32', 'i64', 'isize',
    'f32', 'f64', 'bool', 'str', 'String', 'Vec', 'Box', 'Arc', 'Rc',
    'Mutex', 'RwLock', 'Option', 'Result', 'Cell', 'RefCell',
    # C# primitives + decorators
    'string', 'int', 'uint', 'long', 'ulong', 'short', 'ushort',
    'byte', 'sbyte', 'char', 'double', 'float', 'decimal', 'void',
    'object', 'dynamic', 'nint', 'nuint', 'IntPtr', 'UIntPtr',
    'DllImport', 'MarshalAs', 'StructLayout', 'FieldOffset',
    'Required', 'Optional', 'Serializable',
    # Framework base types we can't refactor
    'View', 'Window', 'object', 'Type',
}
print(f"\nTop 15 god nodes by degree (type-noise filtered):")
filtered = []
for nid, d in sorted(deg.items(), key=lambda x: -x[1]):
    n = node_by_id.get(nid) if False else None  # node_by_id defined below
    break
node_by_id = {n.get('id'): n for n in nodes}
for nid, d in sorted(deg.items(), key=lambda x: -x[1]):
    n = node_by_id.get(nid, {})
    lbl = n.get('label', '?')
    if lbl in TYPE_NOISE:
        continue
    filtered.append((nid, d, n))
    if len(filtered) >= 15:
        break
for nid, d, n in filtered:
    print(f"  {d:4d}  {n.get('label','?'):40s}  {src_of(n)[:80]}")
