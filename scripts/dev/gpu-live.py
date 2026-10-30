"""Live chart of the NVIDIA GPU: temperature, clock, power, use. Reads nvidia-smi once a second.

    python scripts/dev/gpu-live.py
"""
import collections
import subprocess
import threading
import tkinter as tk

SECONDS = 600
QUERY = "temperature.gpu,clocks.gr,clocks.max.gr,power.draw,enforced.power.limit,utilization.gpu,clocks_throttle_reasons.active"
# Label, column, top of the scale, colour, unit.
SERIES = [
    ("Temp", 0, 100, "#e5484d", "C"),
    ("Clock", 1, 2100, "#3b82f6", "MHz"),
    ("Power", 3, 45, "#f5a524", "W"),
    ("Use", 5, 100, "#30a46c", "%"),
]
REASONS = {0x4: "POWER CAP", 0x20: "THERMAL", 0x40: "HW THERMAL", 0x80: "POWER BRAKE"}

samples = collections.deque(maxlen=SECONDS)


def number(text):
    try:
        return float(text)
    except ValueError:
        return 0.0


def read():
    process = subprocess.Popen(
        ["nvidia-smi", f"--query-gpu={QUERY}", "--format=csv,noheader,nounits", "-l", "1"],
        stdout=subprocess.PIPE,
        text=True,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    for line in process.stdout:
        parts = [p.strip() for p in line.split(",")]
        if len(parts) == 7:
            samples.append(([number(p) for p in parts[:6]], int(parts[6], 16)))


root = tk.Tk()
root.title("GPU live")
canvas = tk.Canvas(root, width=760, height=460, bg="#111318", highlightthickness=0)
canvas.pack(fill="both", expand=True)


def draw():
    canvas.delete("all")
    width, height = canvas.winfo_width(), canvas.winfo_height()
    rows = len(SERIES)
    row_height = (height - 30) / rows
    if samples:
        last, reasons = samples[-1]
        active = [name for bit, name in REASONS.items() if reasons & bit]
        canvas.create_text(10, 15, anchor="w", fill="#ff6b6b" if active else "#8b949e",
                           font=("Segoe UI", 11, "bold"),
                           text="LIMIT: " + ", ".join(active) if active else "no limit active")
        canvas.create_text(width - 10, 15, anchor="e", fill="#8b949e", font=("Segoe UI", 10),
                           text=f"limit {last[4]:.0f} W, max clock {last[2]:.0f} MHz, last {SECONDS // 60} min")
    for row, (label, column, top, colour, unit) in enumerate(SERIES):
        y0 = 30 + row * row_height
        y1 = y0 + row_height - 8
        canvas.create_rectangle(0, y0, width, y1, fill="#191c22", outline="")
        points = []
        for i, (values, _) in enumerate(samples):
            x = width - (len(samples) - 1 - i) * width / SECONDS
            y = y1 - min(values[column] / top, 1.0) * (y1 - y0)
            points += [x, y]
        if len(points) >= 4:
            canvas.create_line(points, fill=colour, width=2)
        value = samples[-1][0][column] if samples else 0
        canvas.create_text(10, y0 + 16, anchor="w", fill=colour, font=("Segoe UI", 15, "bold"),
                           text=f"{label}  {value:.0f} {unit}")
    root.after(1000, draw)


threading.Thread(target=read, daemon=True).start()
draw()
root.mainloop()
