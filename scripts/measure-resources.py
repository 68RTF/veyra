#!/usr/bin/env python3
"""Measure the installed Veyra and its mpv child using Linux /proc."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import re
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time

TICKS_PER_SECOND = os.sysconf("SC_CLK_TCK")
KIB_PER_MIB = 1024


def wait_for(description, probe, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = probe()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError(f"Не удалось подтвердить: {description}")


def make_fixture(root, seconds):
    # Keep resource measurements independent of provider availability. Newer
    # builds may look for word-level upgrades even when a local LRC exists.
    tools = root / "bin"
    tools.mkdir()
    curl = tools / "curl"
    curl.write_text("#!/bin/sh\nprintf '{}\\n404'\n", encoding="utf-8")
    curl.chmod(0o755)
    track = root / "track.mp3"
    subprocess.run(
        [
            "ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi",
            "-i", "anoisesrc=color=pink:amplitude=0.08:sample_rate=44100:seed=1",
            "-ac", "2", "-t", str(seconds + 30), "-c:a", "libmp3lame", "-b:a", "192k",
            "-metadata", "title=Тестовый трек", "-metadata", "artist=Veyra",
            str(track),
        ],
        check=True,
    )
    subprocess.run(
        [
            "ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi",
            "-i", "color=c=0x30515b:s=768x768", "-frames:v", "1",
            "-threads", "1", str(root / "cover.png"),
        ],
        check=True,
    )
    track.with_suffix(".lrc").write_text("[00:00]Тестовая строка\n", encoding="utf-8")
    session = root / "data/own-player"
    session.mkdir(parents=True)
    (session / "session.json").write_text(json.dumps({
        "paths": [str(track)],
        "current_path": str(track),
        "shuffle": False,
        "repeat": 0,
        "online_covers": False,
        "volume": 0,
        "muted": True,
    }), encoding="utf-8")


def window_id():
    result = subprocess.run(
        ["xwininfo", "-name", "Veyra"], capture_output=True, text=True
    )
    match = re.search(r"Window id: (0x[0-9a-f]+)", result.stdout)
    if match and "Map State: IsViewable" in result.stdout:
        return int(match[1], 16)
    return None


def press_space(window):
    x11 = ctypes.CDLL("libX11.so.6")
    xtst = ctypes.CDLL("libXtst.so.6")
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XSetInputFocus.argtypes = [
        ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong
    ]
    x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    x11.XKeysymToKeycode.restype = ctypes.c_ubyte
    xtst.XTestFakeKeyEvent.argtypes = [
        ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong
    ]
    display = x11.XOpenDisplay(None)
    if not display:
        raise RuntimeError("Не удалось подключиться к дисплею Xvfb")
    try:
        x11.XSetInputFocus(display, window, 2, 0)
        space = x11.XKeysymToKeycode(display, 0x20)
        if not space or not all(
            xtst.XTestFakeKeyEvent(display, space, pressed, 0) for pressed in (1, 0)
        ):
            raise RuntimeError("XTest не отправил клавишу Space")
        x11.XSync(display, 0)
    finally:
        x11.XCloseDisplay(display)


def property_value(path, name):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(2)
        connection.connect(str(path))
        request = {"command": ["get_property", name], "request_id": 1}
        connection.sendall((json.dumps(request) + "\n").encode())
        with connection.makefile() as replies:
            for line in replies:
                reply = json.loads(line)
                if reply.get("request_id") == 1:
                    return reply.get("data")


def mpv_pid(app_pid):
    children = Path(f"/proc/{app_pid}/task/{app_pid}/children").read_text().split()
    for child in children:
        try:
            if Path(f"/proc/{child}/comm").read_text().strip() == "mpv":
                return int(child)
        except FileNotFoundError:
            continue
    return None


def cpu_ticks(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12])


def memory(pid):
    lines = Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()
    values = {}
    for line in lines:
        if line.startswith(("Rss:", "Pss:")):
            name, amount, _ = line.split()
            values[name] = int(amount)
    return values["Rss:"] / KIB_PER_MIB, values["Pss:"] / KIB_PER_MIB


def measure(pids, seconds):
    start = time.monotonic()
    initial = [cpu_ticks(pid) for pid in pids]
    samples = []
    next_sample = start
    while next_sample < start + seconds:
        time.sleep(max(0, next_sample - time.monotonic()))
        samples.append([memory(pid) for pid in pids])
        next_sample += 1
    time.sleep(max(0, start + seconds - time.monotonic()))
    elapsed = time.monotonic() - start
    cpu = [(cpu_ticks(pid) - ticks) * 100 / (elapsed * TICKS_PER_SECOND)
           for pid, ticks in zip(pids, initial)]
    totals = [tuple(map(sum, zip(*sample))) for sample in samples]
    memory_samples = [*zip(*samples), totals]
    return [
        (cpu_percent, *(statistics.median(values) for values in zip(*proc_samples)))
        for cpu_percent, proc_samples in zip([*cpu, sum(cpu)], memory_samples)
    ]


def benchmark(binary, seconds, root):
    make_fixture(root, seconds)
    with subprocess.Popen(
        [binary], cwd=root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    ) as app:
        try:
            window = wait_for("окно Veyra", window_id)
            child = wait_for("дочерний процесс mpv", lambda: mpv_pid(app.pid))
            ipc = wait_for(
                "IPC mpv",
                lambda: next(root.glob(f"own-player-{app.pid}-*/mpv.sock"), None),
            )
            pids = (app.pid, child)
            wait_for("нулевая громкость mpv", lambda: property_value(ipc, "volume") == 0)
            time.sleep(2)
            if property_value(ipc, "idle-active") is not True:
                raise RuntimeError("mpv уже воспроизводит трек перед замером простоя")
            print("Замер простоя…", flush=True)
            idle = measure(pids, seconds)
            press_space(window)
            wait_for("начало воспроизведения", lambda: (property_value(ipc, "time-pos") or 0) > 0.25)
            backend = wait_for("аудиовыход mpv", lambda: property_value(ipc, "current-ao"))
            time.sleep(2)
            position = property_value(ipc, "time-pos")
            print("Замер воспроизведения…", flush=True)
            playing = measure(pids, seconds)
            after = property_value(ipc, "time-pos")
            if after is None or after - position < seconds * 0.85:
                raise RuntimeError("Трек не воспроизводился на протяжении замера")
            press_space(window)
            wait_for("паузу mpv", lambda: property_value(ipc, "pause") is True)
            time.sleep(2)
            print("Замер паузы…", flush=True)
            paused = measure(pids, seconds)
            if property_value(ipc, "pause") is not True:
                raise RuntimeError("Воспроизведение возобновилось во время замера паузы")
            print(f"Окно GTK3 в Xvfb, MP3 192 кбит/с, аудиовыход mpv: {backend}, {seconds} с на режим.")
            print("CPU — % одного ядра, RSS/PSS — медианы в МиБ.")
            print(f"{'Режим':<18} {'Процесс':<8} {'CPU, %':>8} {'RSS, МиБ':>10} {'PSS, МиБ':>10}")
            for phase, rows in (("Простой", idle), ("Воспроизведение", playing), ("Пауза", paused)):
                for name, (cpu, rss, pss) in zip(("Veyra", "mpv", "Всего"), rows):
                    print(f"{phase:<18} {name:<8} {cpu:8.2f} {rss:10.1f} {pss:10.1f}")
        finally:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    app.kill()


def main():
    parser = argparse.ArgumentParser(
        description="Измеряет CPU, RSS и PSS Veyra и mpv в изолированном Xvfb."
    )
    parser.add_argument("--binary", default="veyra", help="путь к бинарнику или имя из PATH")
    parser.add_argument("--seconds", type=int, default=15, help="секунд на каждый режим, минимум 5")
    parser.add_argument("--fixture", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.seconds < 5:
        parser.error("--seconds: минимум 5")
    binary = shutil.which(args.binary)
    if binary is None:
        parser.error("бинарник Veyra не найден")
    binary = str(Path(binary).resolve())
    if args.fixture is not None:
        benchmark(binary, args.seconds, args.fixture)
        return
    for command in ("xvfb-run", "Xvfb", "xauth", "xwininfo", "dbus-run-session", "ffmpeg", "mpv"):
        if shutil.which(command) is None:
            parser.error(f"в PATH не найден {command}")
    with tempfile.TemporaryDirectory(prefix="veyra-bench-", dir="/tmp") as folder:
        root = Path(folder)
        env = os.environ.copy()
        env.pop("WAYLAND_DISPLAY", None)
        env.update(
            GDK_BACKEND="x11", GTK_USE_PORTAL="0", GIO_USE_VFS="local",
            GTK_THEME="Adwaita:dark", XDG_CURRENT_DESKTOP="GTK", TMPDIR=folder,
            XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
            XDG_CONFIG_HOME=str(root / "config"),
            PATH=str(root / "bin") + os.pathsep + env.get("PATH", ""),
        )
        result = subprocess.run([
            "xvfb-run", "-a", "-s", "-screen 0 1280x900x24",
            "dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()),
            "--binary", binary, "--seconds", str(args.seconds), "--fixture", folder,
        ], env=env, text=True, stderr=subprocess.PIPE)
        if result.returncode:
            details = next(
                (line for line in reversed(result.stderr.splitlines())
                 if line.startswith(("Замер прерван:", "xvfb-run: error:", "dbus-run-session:"))),
                f"процесс замера завершился с кодом {result.returncode}",
            )
            sys.exit(details)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(f"Замер прерван: {error}")
