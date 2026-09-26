#!/usr/bin/env python3

import argparse
from concurrent.futures import ThreadPoolExecutor
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import time


METRICS = ("Pss", "Rss", "Private_Clean", "Private_Dirty", "SwapPss")


def process_table():
    processes = {}
    for directory in Path("/proc").iterdir():
        if not directory.name.isdecimal():
            continue
        try:
            fields = (directory / "stat").read_text().rsplit(")", 1)[1].split()
            processes[int(directory.name)] = {
                "parent": int(fields[1]),
                "session": int(fields[3]),
                "start": int(fields[19]),
                "state": fields[0],
            }
        except (FileNotFoundError, ProcessLookupError):
            continue
    return processes


def owned_processes(root, known):
    processes = process_table()
    selected = {
        process_id
        for process_id, info in processes.items()
        if process_id == root
        or info["session"] == root
        or known.get(process_id) == info["start"]
    }
    while True:
        children = {
            process_id
            for process_id, info in processes.items()
            if info["parent"] in selected
        }
        if children.issubset(selected):
            break
        selected.update(children)
    for process_id in selected:
        known[process_id] = processes[process_id]["start"]
    return {
        process_id: processes[process_id]
        for process_id in selected
        if processes[process_id]["state"] != "Z"
    }


def memory_sample(process, known):
    if process.poll() is not None:
        raise RuntimeError(f"Editor exited with status {process.returncode}")
    measurements = []
    for process_id in sorted(owned_processes(process.pid, known)):
        directory = Path("/proc") / str(process_id)
        try:
            rollup = (directory / "smaps_rollup").read_text()
            command = (directory / "cmdline").read_bytes().replace(b"\0", b" ")
        except (FileNotFoundError, ProcessLookupError):
            if process_id == process.pid:
                raise RuntimeError("Editor disappeared during measurement")
            continue
        values = {
            key: int(value)
            for key, value in re.findall(r"^(\w+):\s+(\d+) kB$", rollup, re.M)
        }
        missing = set(METRICS) - values.keys()
        if missing:
            raise RuntimeError(f"Missing memory counters for {process_id}: {missing}")
        measurements.append({
            "pid": process_id,
            "command": command.decode(errors="replace").strip(),
            **{metric: values[metric] for metric in METRICS},
        })
    if not any(item["pid"] == process.pid for item in measurements):
        raise RuntimeError("No memory measurement for editor process")
    return {
        "processes": measurements,
        **{metric: sum(item[metric] for item in measurements) for metric in METRICS},
    }


def stop_editor(process, known):
    for stop_signal in (signal.SIGTERM, signal.SIGKILL):
        for process_id, info in owned_processes(process.pid, known).items():
            try:
                current = process_table().get(process_id)
                if current and current["start"] == info["start"]:
                    os.kill(process_id, stop_signal)
            except ProcessLookupError:
                continue
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            process.poll()
            if not owned_processes(process.pid, known):
                process.wait(timeout=1)
                return
            time.sleep(0.1)
    raise RuntimeError(f"Could not stop all editor processes for {process.pid}")


def visible_windows(process_id, environment):
    result = subprocess.run(
        ["xdotool", "search", "--onlyvisible", "--pid", str(process_id)],
        env=environment, capture_output=True, text=True, check=False,
    )
    if result.returncode not in (0, 1):
        raise RuntimeError(f"Window lookup failed: {result.stderr}")
    return result.stdout.split()


def prepare_workload(directory, arguments):
    project = directory / "workload"
    project.mkdir()
    archive = subprocess.check_output(["git", "-C", str(arguments.repository), "archive", "HEAD"])
    subprocess.run(["tar", "-x", "-C", str(project)], input=archive, check=True)
    files = [
        "crates/rope/src/rope.rs", "crates/rope/src/chunk.rs",
        "crates/rope/src/point.rs", "crates/rope/src/point_utf16.rs",
        "crates/rope/src/offset_utf16.rs", "crates/rope/src/unclipped.rs",
        "crates/editor/src/editor.rs", "crates/workspace/src/workspace.rs",
    ]
    files.extend(str(path.relative_to(project)) for path in sorted((project / "crates/languages/src").glob("*.rs"))[:12])
    (project / "benchmark-edit.rs").write_text("fn main() {}\n")
    with (project / "benchmark-large.log").open("w") as output:
        for number in range(200000):
            output.write(f"2026-09-19T00:00:00Z INFO request={number:06d} method=GET route=/api/projects status=200 duration_ms=12 benchmark_record\n")
    subprocess.run(["git", "init", "-q", str(project)], check=True)
    subprocess.run(["git", "-C", str(project), "add", "."], check=True)
    subprocess.run([
        "git", "-C", str(project), "-c", "user.name=Memory Benchmark",
        "-c", "user.email=benchmark@localhost", "-c", "commit.gpgsign=false",
        "commit", "-qm", "Benchmark fixture",
    ], check=True)
    manifest = {"files": files, "scenario": arguments.scenario, "source_sha256": {}}
    for path in sorted(project.rglob("*")):
        if path.is_file() and ".git" not in path.relative_to(project).parts:
            with path.open("rb") as source:
                manifest["source_sha256"][str(path.relative_to(project))] = hashlib.file_digest(source, "sha256").hexdigest()
    manifest["repository_commit"] = subprocess.check_output(
        ["git", "-C", str(arguments.repository), "rev-parse", "HEAD"], text=True,
    ).strip()
    (directory / "workload.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return project, files


class Workload:
    def __init__(self, process, environment, project, files, directory, arguments, vscode=False):
        self.process = process
        self.environment = environment
        self.project = project
        self.files = files
        self.directory = directory
        self.arguments = arguments
        self.vscode = vscode
        self.window = visible_windows(process.pid, environment)[0]
        executable = Path(os.readlink(f"/proc/{process.pid}/exe"))
        self.launcher = executable.parent / "bin/code" if vscode else executable.parent.parent / "bin" / executable.name.removesuffix("-editor")

    def keys(self, *keys):
        subprocess.run(
            ["xdotool", "windowfocus", "--sync", self.window,
             "key", "--clearmodifiers", "--delay", "70", *keys],
            env=self.environment, check=True, timeout=15,
        )
        time.sleep(0.2)

    def type_text(self, value, terminal=False):
        subprocess.run(
            ["xclip", "-selection", "clipboard"], input=value.encode(),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            env=self.environment, check=True, timeout=10,
        )
        self.keys("ctrl+shift+v" if terminal else "ctrl+v")

    def title(self):
        return subprocess.check_output(
            ["xdotool", "getwindowname", self.window], env=self.environment, text=True,
        ).strip()

    def screenshot(self, name):
        subprocess.run(
            ["import", "-window", "root", str(self.directory / f"{name}.png")],
            env=self.environment, check=True, timeout=20,
        )

    def wait_for(self, predicate, description, timeout=15):
        deadline = time.monotonic() + timeout
        while not predicate():
            if time.monotonic() > deadline:
                self.screenshot("validation-failure")
                raise RuntimeError(description)
            time.sleep(0.2)

    def open_file(self, relative_path):
        self.keys("Escape")
        command = [str(self.launcher), "--user-data-dir", str(self.directory / "profile")]
        command.extend(["--reuse-window", "--no-sandbox"] if self.vscode else ["--existing"])
        command.append(str(self.project / relative_path))
        subprocess.run(
            command,
            env=self.environment, check=True, timeout=30,
        )
        self.wait_for(
            lambda: Path(relative_path).name in self.title(),
            f"File did not open: {relative_path}", timeout=30,
        )
        time.sleep(0.3)

    def multiple_files(self):
        for relative_path in self.files:
            self.open_file(relative_path)
        self.screenshot("multiple-files")

    def edit(self):
        relative_path = "benchmark-edit.rs"
        self.open_file(relative_path)
        for number in range(20):
            self.keys("ctrl+End", "Return")
            self.type_text(f"// benchmark_edit_{number:02d}")
            self.keys("ctrl+s")
            self.wait_for(
                lambda: f"// benchmark_edit_{number:02d}" in (self.project / relative_path).read_text(),
                f"Edit {number} was not saved",
            )
            self.keys("Prior", "Next")
        self.wait_for(
            lambda: all(f"// benchmark_edit_{number:02d}" in (self.project / relative_path).read_text() for number in range(20)),
            "Editing workload did not save all expected edits",
        )
        self.screenshot("editing")

    def search(self):
        for query in ("pub struct Rope", "fn offset_to_point", "fn focus_handle"):
            self.keys("Escape", "ctrl+shift+f")
            time.sleep(0.3)
            self.keys("ctrl+a")
            self.type_text(query)
            time.sleep(1)
            self.keys("Return")
            time.sleep(5)
        self.screenshot("search-results")
        self.keys("Escape")

    def large_file(self):
        self.open_file("benchmark-large.log")
        for number in range(10):
            self.keys("ctrl+End", "Prior", "ctrl+Home", "Next")
        self.keys("ctrl+f")
        self.type_text("request=123456")
        self.keys("Return", "Escape")
        self.screenshot("large-file")

    def terminal(self):
        marker = self.directory / "terminal-complete"
        script = self.directory / "terminal-workload.py"
        script.write_text(
            "import pathlib\n"
            "for number in range(20000):\n"
            "    print(f'{number:06d} INFO processing request: status=200 elapsed=12ms')\n"
            f"pathlib.Path({str(marker)!r}).write_text('success')\n"
        )
        self.keys("Escape", "ctrl+grave")
        time.sleep(1)
        self.type_text(f"{shlex.quote(sys.executable)} {shlex.quote(str(script))}", terminal=True)
        self.keys("Return")
        deadline = time.monotonic() + 90
        while not marker.exists():
            if time.monotonic() > deadline:
                self.screenshot("terminal-failure")
                raise RuntimeError("Terminal workload did not complete successfully")
            time.sleep(0.2)
        time.sleep(1)
        self.screenshot("terminal-output")

    def recover(self):
        self.keys("ctrl+grave", "Escape", "ctrl+k", "ctrl+w" if self.vscode else "w")
        time.sleep(2)
        self.screenshot("closed-tabs")


def measure_phase(name, action, process, known, arguments, directory, started):
    samples = []
    def capture(kind):
        sample = memory_sample(process, known)
        sample.update(seconds_since_launch=time.monotonic() - started, kind=kind)
        samples.append(sample)

    with ThreadPoolExecutor(max_workers=1) as executor:
        future = executor.submit(action)
        while not future.done():
            capture("active")
            time.sleep(arguments.interval)
        future.result()
    deadline = time.monotonic() + arguments.phase_settle
    while time.monotonic() < deadline:
        capture("settling")
        time.sleep(arguments.interval)
    for sample_number in range(arguments.samples):
        capture("settled")
        if sample_number + 1 < arguments.samples:
            time.sleep(arguments.interval)
    result = {
        "phase": name, "samples": samples,
        "median_kib": {
            metric: statistics.median(sample[metric] for sample in samples if sample["kind"] == "settled")
            for metric in METRICS
        },
        "peak_kib": {metric: max(sample[metric] for sample in samples) for metric in METRICS},
    }
    (directory / f"phase-{name}.json").write_text(json.dumps(result, indent=2) + "\n")
    subprocess.run(
        ["import", "-window", "root", str(directory / f"phase-{name}.png")],
        check=True, timeout=20,
    )
    print(f"  {name}: settled PSS {result['median_kib']['Pss'] / 1024:.1f} MiB; sampled peak {result['peak_kib']['Pss'] / 1024:.1f} MiB", flush=True)
    return result


def measure(binary, name, run_number, arguments, output):
    directory = output / f"{run_number:02d}-{name}"
    directory.mkdir()
    profile = directory / "profile"
    vscode = name == "VSCode"
    configuration = profile / ("User" if vscode else "config")
    configuration.mkdir(parents=True)
    # Freeze release versions and prevent benchmark telemetry from being sent.
    settings = {"auto_update": False, "telemetry": {"diagnostics": False, "metrics": False}}
    project, files = (arguments.path, [])
    if arguments.scenario != "idle":
        project, files = prepare_workload(directory, arguments)
        settings.update({
            "preview_tabs": {"enabled": False}, "format_on_save": "off",
            "enable_language_server": False,
            "terminal": {"max_scroll_history_lines": 10000},
        })
    if arguments.trust_project or arguments.scenario != "idle":
        settings["session"] = {"trust_all_worktrees": True}
    if vscode:
        settings = {
            "update.mode": "none", "telemetry.telemetryLevel": "off",
            "extensions.autoUpdate": False, "extensions.autoCheckUpdates": False,
            "workbench.startupEditor": "none" if project else "welcomePage",
            "workbench.welcomePage.experimentalOnboarding": False,
            "workbench.editor.enablePreview": False, "editor.formatOnSave": False,
            "terminal.integrated.scrollback": 10000,
            "security.workspace.trust.enabled": not (arguments.trust_project or project),
        }
    (configuration / "settings.json").write_text(json.dumps(settings, indent=2) + "\n")
    environment = dict(os.environ)
    environment.pop("WAYLAND_DISPLAY", None)
    for category in ("CONFIG", "DATA", "CACHE", "STATE"):
        environment[f"XDG_{category}_HOME"] = str(directory / f"xdg-{category.lower()}")
    environment.update({
        "ZED_STATELESS": "1",
        "ZED_WINDOW_SIZE": "1280,800",
        "ZED_WINDOW_POSITION": "0,0",
    })
    if arguments.scenario != "idle":
        # The per-profile CLI connection avoids racing the file picker's search UI.
        environment.pop("ZED_STATELESS", None)
    command = [str(binary), "--user-data-dir", str(profile)]
    if vscode:
        environment["LIBGL_ALWAYS_SOFTWARE"] = "1"
        environment.pop("ELECTRON_RUN_AS_NODE", None)
        command.extend([
            "--extensions-dir", str(directory / "extensions"), "--new-window",
            "--shared-data-dir", str(directory / "shared-data"),
            "--no-sandbox", "--skip-release-notes", "--ozone-platform=x11",
            "--use-gl=angle", "--use-angle=gl", "--ignore-gpu-blocklist=true",
        ])
    if project:
        if vscode:
            command.append("--")
        command.append(str(project))
    known = {}
    with (directory / "stdout.log").open("w") as log:
        process = subprocess.Popen(
            command, env=environment, cwd=directory,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        started = time.monotonic()
        try:
            deadline = started + arguments.window_timeout
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"{name} exited; see {directory / 'stdout.log'}")
                owned_processes(process.pid, known)
                windows = visible_windows(process.pid, environment)
                if windows:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"{name} did not open a visible window")
                time.sleep(0.2)
            for window in windows:
                subprocess.run(
                    ["xdotool", "windowsize", "--sync", window, "1280", "800"],
                    env=environment, check=True, timeout=10,
                )
                if vscode:
                    subprocess.run(
                        ["xdotool", "windowmove", "--sync", window, "0", "0"],
                        env=environment, check=True, timeout=10,
                    )
            if not project and not vscode:
                time.sleep(2)
                for window in windows:
                    subprocess.run(
                        ["xdotool", "windowfocus", "--sync", window,
                         "key", "--clearmodifiers", "ctrl+Return"],
                        env=environment, check=True, timeout=10,
                    )
            if vscode and project:
                workload = Workload(process, environment, project, files, directory, arguments, vscode)
                workload.wait_for(
                    lambda: project.name in workload.title(),
                    "VS Code did not open the benchmark project", timeout=30,
                )
            deadline = time.monotonic() + arguments.settle
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"{name} exited while settling")
                owned_processes(process.pid, known)
                time.sleep(min(0.2, max(0, deadline - time.monotonic())))
            if arguments.scenario != "idle":
                workload = Workload(process, environment, project, files, directory, arguments, vscode)
                actions = [
                    ("multiple_files", workload.multiple_files), ("editing", workload.edit),
                    ("project_search", workload.search), ("large_file", workload.large_file),
                    ("terminal", workload.terminal), ("closed_tabs", workload.recover),
                ]
                phases = [
                    measure_phase(phase_name, action, process, known, arguments, directory, started)
                    for phase_name, action in actions
                ]
                result = {"editor": name, "run": run_number, "command": command, "phases": phases}
                (directory / "measurements.json").write_text(json.dumps(result, indent=2) + "\n")
                return result
            samples = []
            for sample_number in range(arguments.samples):
                sample = memory_sample(process, known)
                sample["seconds_since_launch"] = time.monotonic() - started
                samples.append(sample)
                if sample_number + 1 < arguments.samples:
                    time.sleep(arguments.interval)
            windows = visible_windows(process.pid, environment)
            if not windows:
                raise RuntimeError(f"{name} has no visible window after sampling")
            window_tree = subprocess.check_output(
                ["xwininfo", "-root", "-tree"], env=environment, text=True,
            )
            (directory / "windows.txt").write_text(window_tree)
            if shutil.which("import"):
                subprocess.run(
                    ["import", "-window", "root", str(directory / "screenshot.png")],
                    env=environment, check=True, timeout=20,
                )
            result = {
                "editor": name, "run": run_number, "command": command,
                "samples": samples,
                "median_kib": {
                    metric: statistics.median(sample[metric] for sample in samples)
                    for metric in METRICS
                },
            }
            (directory / "measurements.json").write_text(json.dumps(result, indent=2) + "\n")
            return result
        finally:
            stop_editor(process, known)


def main():
    parser = argparse.ArgumentParser(
        description="Compare total editor process-tree PSS and RSS on Linux.",
        epilog="Run under xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session --. "
        "Use actual libexec/*-editor or VS Code's top-level code binary, not CLI launchers. "
        "PSS apportions shared pages; RSS can count them multiple times. "
        "The display server, harness, unmapped file cache and kernel memory are excluded.",
    )
    parser.add_argument("--runs", type=int, default=int(os.getenv("RUNS", "5")))
    parser.add_argument("--settle", type=float, default=float(os.getenv("SETTLE_SECONDS", "30")))
    parser.add_argument("--samples", type=int, default=10)
    parser.add_argument("--interval", type=float, default=1)
    parser.add_argument("--window-timeout", type=float, default=60)
    parser.add_argument("--scenario", choices=("idle", "workspace"), default="idle")
    parser.add_argument("--phase-settle", type=float, default=5)
    parser.add_argument("--repository", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--path", type=Path, default=os.getenv("OPEN_PATH"))
    parser.add_argument("--trust-project", action="store_true", help="Trust the supplied project in the isolated benchmark profiles")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--wu", type=Path, default=os.getenv("WU_BIN", str(Path.home() / ".local/wu.app/libexec/wu-editor")))
    parser.add_argument("--zed", type=Path, default=os.getenv("ZED_BIN", str(Path.home() / ".local/zed.app/libexec/zed-editor")))
    parser.add_argument("--vscode", type=Path, default=os.getenv("VSCODE_BIN", str(Path.home() / ".local/vscode/code")))
    parser.add_argument("--editors", nargs="+", choices=("Wu", "Zed", "VSCode"), default=["Wu", "Zed"])
    arguments = parser.parse_args()
    if arguments.runs < 1 or arguments.samples < 1 or arguments.settle < 0 or arguments.interval <= 0:
        parser.error("Runs/samples/interval must be positive; settle must be nonnegative")
    if not os.getenv("DISPLAY"):
        parser.error("An X11 display is required; use xvfb-run (see --help)")
    for executable in ("xdotool", "xwininfo"):
        if not shutil.which(executable):
            parser.error(f"Missing required command: {executable}")
    available = {"Wu": arguments.wu, "Zed": arguments.zed, "VSCode": arguments.vscode}
    binaries = {name: available[name].resolve() for name in arguments.editors}
    for binary in binaries.values():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"Not executable: {binary}")
    if arguments.path:
        arguments.path = arguments.path.resolve(strict=True)
    if arguments.scenario != "idle":
        for executable in ("import", "xclip"):
            if not shutil.which(executable):
                parser.error(f"Active workloads require {executable}")
    timestamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = (arguments.output or Path.home() / ".local/state/wu-memory-benchmark" / timestamp).resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {
        "timestamp_utc": timestamp,
        "system": platform.uname()._asdict(),
        "settings": {key: str(value) if isinstance(value, Path) else value for key, value in vars(arguments).items()},
        "environment": {key: os.environ.get(key) for key in ("DISPLAY", "VK_ICD_FILENAMES", "ZED_ALLOW_ROOT", "ZED_ALLOW_EMULATED_GPU")},
        "metric_units": "KiB", "results": [],
        "binaries": {},
    }
    for name, binary in binaries.items():
        with binary.open("rb") as executable:
            digest = hashlib.file_digest(executable, "sha256").hexdigest()
        if name == "VSCode":
            launcher = binary.parent / "bin/code"
            version_command = [str(launcher), "--no-sandbox", "--user-data-dir", str(output / "version-profile"), "--version"]
        else:
            launcher = binary.parent.parent / "bin" / name.lower()
            version_command = [str(launcher), "--version"]
        version = subprocess.check_output(version_command, text=True, timeout=15).strip() if launcher.is_file() else None
        report["binaries"][name] = {"path": str(binary), "sha256": digest, "version": version}
    report_path = output / "results.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Artifacts: {output}", flush=True)
    for run_number in range(1, arguments.runs + 1):
        order = list(binaries) if run_number % 2 else list(reversed(binaries))
        for name in order:
            print(f"Run {run_number}/{arguments.runs}: {name}", flush=True)
            result = measure(binaries[name], name, run_number, arguments, output)
            report["results"].append(result)
            report_path.write_text(json.dumps(report, indent=2) + "\n")
            if arguments.scenario == "idle":
                print("  " + ", ".join(f"{metric} {result['median_kib'][metric] / 1024:.1f} MiB" for metric in ("Pss", "Rss", "SwapPss")), flush=True)
    if arguments.scenario != "idle":
        report["summary"] = {
            phase_name: {
                name: {
                    statistic: {
                        metric: statistics.median(
                            phase[statistic][metric]
                            for result in report["results"] if result["editor"] == name
                            for phase in result["phases"] if phase["phase"] == phase_name
                        ) for metric in METRICS
                    } for statistic in ("median_kib", "peak_kib")
                } for name in binaries
            } for phase_name in (phase["phase"] for phase in report["results"][0]["phases"])
        }
        report_path.write_text(json.dumps(report, indent=2) + "\n")
        print("\nMedian of run medians / median of sampled peaks (MiB PSS):")
        for phase_name, editors in report["summary"].items():
            values = "; ".join(
                f"{name} {values['median_kib']['Pss'] / 1024:.1f} / {values['peak_kib']['Pss'] / 1024:.1f}"
                for name, values in editors.items()
            )
            print(f"{phase_name}: {values}")
        return
    report["summary"] = {
        name: {
            metric: statistics.median(result["median_kib"][metric] for result in report["results"] if result["editor"] == name)
            for metric in METRICS
        }
        for name in binaries
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    print("\nMedian of run medians:")
    for name, summary in report["summary"].items():
        print(f"{name}: PSS {summary['Pss'] / 1024:.1f} MiB; RSS {summary['Rss'] / 1024:.1f} MiB")
    if "Wu" in report["summary"] and "Zed" in report["summary"]:
        difference = (1 - report["summary"]["Wu"]["Pss"] / report["summary"]["Zed"]["Pss"]) * 100
        print(f"Wu PSS reduction relative to Zed: {difference:.1f}%")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
