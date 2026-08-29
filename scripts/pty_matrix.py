#!/usr/bin/env python3
"""Real-CLI PTY verification harness for opencode2api (project deploy gate).

Drives the REAL `claude` CLI in a pseudo-terminal against a NON-PRODUCTION
bridge instance fed by the OpenAI-compatible SSE stub, and asserts the
mandatory manual-verification matrix from CLAUDE.md:

  1. two_consecutive      - two consecutive requests both round-trip
  2. agent_tool_call      - a tool call that invokes an agent (+ continuation)
  3. streaming_tool_call  - fragmented streaming tool call (+ continuation)
  4. shell_command        - `!` shell command: TUI bash-mode (zero upstream)
                            AND bridge-side `!` interception over direct HTTP
  5. ctrlc_midstream      - Ctrl+C mid-stream, clean terminal, REPL survives
  6. upstream_error_retry - stub 500s the first attempt; CLI retries itself
  7. ten_turns            - >=10 consecutive requests, no terminal dirt
  8. midstream_error_terminates_cleanly
                          - stub ends a STARTED stream with an in-band
                            {"error": ...} data line (no [DONE]); bridge must
                            surface ONE terminal Anthropic error event, the
                            CLI turn must end with NO client re-send, and the
                            REPL must survive

Everything is loopback-only: the bridge binds 127.0.0.1 on a free ephemeral
port, OPENCODE_UPSTREAM_BASE_URL pins all upstream traffic to the local
stub, and an EgressWatcher samples `ss -tnp` every second to prove nothing
leaves localhost. Never touches :4000/:4096 or any production process.

Each scenario runs against a FRESH stub+bridge pair in an isolated temp
HOME/runtime dir (TestBridge-style). Children are spawned in their own
process groups and hard-killed from atexit + signal handlers, so a failure
or Ctrl+C leaves zero processes behind.

Usage:
  python3 scripts/pty_matrix.py list
  python3 scripts/pty_matrix.py smoke                 # scenarios 1 + 4
  python3 scripts/pty_matrix.py run --scenarios all   # full 8-scenario pass
  python3 scripts/pty_matrix.py run --scenarios 1,4 --keep --out DIR

Stdlib only (pexpect/pyte NOT required).
"""
from __future__ import annotations

import argparse
import atexit
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

FORBIDDEN_PORTS = {4000, 4096}
STUB_CANDIDATES = [
    ROOT / "tests" / "stub_openai.py",
    ROOT / "artifacts" / "claude-upstream-reverse" / "tests" / "stub_openai.py",
]
DEFAULT_BRIDGE_BIN = ROOT / "target" / "debug" / "opencode2api-serve"

ANSI_RE = re.compile(
    r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][0-9A-Z]|\x1b[=>]"
)
SPINNER_RE = re.compile("[⠀-⣿◐-◓▁-▏]")
RAW_SSE_RE = re.compile(r"(?m)^\s*(data:\s|event:\s)")

# Dialog auto-answers (fallbacks; the profile ships pre-provisioned
# .claude.json and uses ANTHROPIC_AUTH_TOKEN so most never render).
# Enter accepts the highlighted default. Matching is done against NEW
# terminal text only, and each onboarding needle fires at most once, so
# stale scrollback can never re-trigger keystrokes.
DIALOG_ANSWERS = [
    ("Do you want to use this API key", b"\x1b[A\r"),   # Up -> Yes, Enter
    ("Press Enter to continue", b"\r"),
    ("Is this a project you created", b"\r"),
    ("I trust this folder", b"\r"),
    # Repeatable permission prompts (new text each time):
    ("Do you want to allow Claude to", b"\r"),
    ("Do you want to proceed", b"\r"),
    ("Would you like to approve", b"\r"),
]
ONBOARDING_NEEDLES = {
    "Do you want to use this API key",
    "Press Enter to continue",
    "Is this a project you created",
    "I trust this folder",
}
# Phrases unique to an ACTIVE modal dialog — none survive dismissal into
# scrollback of the (small) settled REPL screen, unlike e.g. "Quick safety
# check", whose text remains on-screen even after the dialog is answered.
READY_BLOCKERS = [
    "Esc to cancel",                    # any modal dialog footer
    "Do you want to use this API key",
    "Press Enter to continue",
    "Let's get started",                # first-run theme picker
]


def strip_ansi(raw: bytes | str) -> str:
    if isinstance(raw, bytes):
        raw = raw.decode("utf-8", errors="replace")
    return ANSI_RE.sub("", raw)


_WS_RE = re.compile(r"\s+")


def squash(text: str) -> str:
    """Whitespace-free form for dialog matching.

    The TUI styles words individually, so ANSI-stripped frames frequently
    lose inter-word spacing ('Isthisaprojectyoucreated'). Matching must be
    whitespace-insensitive to be reliable."""
    return _WS_RE.sub("", text)


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    if port in FORBIDDEN_PORTS:  # astronomically unlikely; guard anyway
        return free_port()
    return port


def count_lines(path: Path) -> int:
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return sum(1 for _ in fh)
    except OSError:
        return 0


class ProcRegistry:
    """Every spawned process (and PTY child pgid) dies here on exit."""

    def __init__(self) -> None:
        self.procs: list[subprocess.Popen] = []
        self.extra_pids: set[int] = set()
        self.temp_dirs: list[Path] = []
        self.lock = threading.Lock()

    def add(self, proc: subprocess.Popen) -> None:
        with self.lock:
            self.procs.append(proc)

    def add_pid(self, pid: int) -> None:
        with self.lock:
            self.extra_pids.add(pid)

    @staticmethod
    def _kill_group(pid: int) -> None:
        try:
            os.killpg(os.getpgid(pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass

    def kill_all(self) -> None:
        with self.lock:
            procs, self.procs = self.procs[:], []
            pids, self.extra_pids = set(self.extra_pids), set()
        for proc in procs:
            self._kill_group(proc.pid)
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        for pid in pids:
            self._kill_group(pid)
        for d in self.temp_dirs:
            shutil.rmtree(d, ignore_errors=True)
        self.temp_dirs.clear()


REGISTRY = ProcRegistry()


def _atexit_cleanup() -> None:
    REGISTRY.kill_all()


def _signal_handler(signum, _frame):
    REGISTRY.kill_all()
    raise SystemExit(128 + signum)


atexit.register(_atexit_cleanup)
for _sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
    signal.signal(_sig, _signal_handler)


def spawn_clean(cmd: list[str], env: dict[str, str], cwd: Path,
                logs: dict[str, str]) -> subprocess.Popen:
    """Spawn cmd in its own session/process group with the given env."""
    proc = subprocess.Popen(
        cmd,
        cwd=str(cwd),
        env=env,
        stdout=open(logs["stdout"], "ab"),
        stderr=open(logs["stderr"], "ab"),
        stdin=subprocess.DEVNULL,
        start_new_session=True,
    )
    REGISTRY.add(proc)
    return proc


def wait_tcp(port: int, deadline_s: float = 15.0) -> bool:
    end = time.monotonic() + deadline_s
    while time.monotonic() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.1)
    return False


class StubUpstream:
    """tests-style OpenAI Chat Completions SSE stub on a free loopback port."""

    def __init__(self, out_dir: Path, stub_path: Path) -> None:
        self.port = free_port()
        self.req_log = out_dir / "stub.req.log"
        self.done_marker = out_dir / "stub.done"
        self.logs = {
            "stdout": str(out_dir / "stub.stdout.log"),
            "stderr": str(out_dir / "stub.stderr.log"),
        }
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
               "HOME": str(out_dir)}
        self.proc = spawn_clean(
            [sys.executable, str(stub_path),
             "--port", str(self.port),
             "--req-log", str(self.req_log),
             "--done-marker", str(self.done_marker)],
            env, out_dir, self.logs,
        )
        if not wait_tcp(self.port):
            raise RuntimeError(
                f"stub did not listen on {self.port}; "
                f"stderr: {Path(self.logs['stderr']).read_text(errors='replace')[-800:]}")
        self.base_url = f"http://127.0.0.1:{self.port}"

    def req_count(self) -> int:
        return count_lines(self.req_log)

    def done_count(self) -> int:
        try:
            with open(self.done_marker, encoding="utf-8") as fh:
                return sum(1 for ln in fh if ln.startswith("DONE "))
        except OSError:
            return 0


class BridgeInstance:
    """Non-production `opencode2api-serve` with TestBridge-style isolation."""

    def __init__(self, out_dir: Path, binary: Path, stub_base: str,
                 shell_policy: str) -> None:
        iso = Path(tempfile.mkdtemp(prefix=f"oc2api-pty-{os.getpid()}-",
                                    dir=str(out_dir)))
        runtime_dir = iso / "runtime"
        home_dir = iso / "home"
        runtime_dir.mkdir()
        home_dir.mkdir()
        self.iso_root = iso
        self.keep = False
        self.port = free_port()
        self.logs = {
            "stdout": str(out_dir / "bridge.stdout.log"),
            "stderr": str(out_dir / "bridge.stderr.log"),
        }
        # Allowlisted environment: ambient repo dotfiles, ~/.opencode2api,
        # proxy pools and API keys cannot leak into the instance under test.
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
            "HOME": str(home_dir),
            "BRIDGE_PORT": str(self.port),
            "BRIDGE_HOST": "127.0.0.1",
            "RUNTIME_DIR": str(runtime_dir),
            "BRIDGE_CONFIG_PATH": str(runtime_dir / "config.toml"),
            # "info" (not "warn"): S8's bridge-side proof is the INFO-level
            # "mid-stream upstream error event" marker emitted by the
            # error_terminated break in forward/stream/execute.rs. Log volume
            # stays negligible at these scenario sizes.
            "RUST_LOG": "info",
            "BRIDGE_AUTH_TOKEN": "",
            "DASHBOARD_ADMIN_TOKEN": "",
            "REST_API_TOKEN": "",
            "OPENCODE_MODEL": "",
            "BRIDGE_PRIMARY_PROXIES": "",
            "BRIDGE_WARM_STANDBY_PROXIES": "",
            "BRIDGE_EGRESS_MODE": "direct",
            # Pin ALL upstream traffic to the local stub.
            "OPENCODE_UPSTREAM_BASE_URL": stub_base,
            "BRIDGE_SHELL_POLICY": shell_policy,
        }
        if not binary.exists():
            raise FileNotFoundError(f"bridge binary missing: {binary} "
                                    "(build with `cargo build` first)")
        self.proc = spawn_clean([str(binary)], env, runtime_dir, self.logs)
        self.base_url = f"http://127.0.0.1:{self.port}"
        deadline = time.monotonic() + 20
        last_err = ""
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                tail = Path(self.logs["stderr"]).read_text(errors="replace")[-2000:]
                raise RuntimeError(f"bridge exited early ({self.proc.returncode}); "
                                   f"stderr tail:\n{tail}")
            try:
                with urllib.request.urlopen(f"{self.base_url}/health/live",
                                            timeout=1) as resp:
                    if resp.status == 200:
                        return
            except Exception as exc:  # noqa: BLE001
                last_err = repr(exc)
            time.sleep(0.15)
        raise RuntimeError(f"bridge never became healthy: {last_err}")

    def post_messages(self, body: dict) -> tuple[int, str]:
        req = urllib.request.Request(
            f"{self.base_url}/v1/messages",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return resp.status, resp.read().decode(errors="replace")
        except urllib.error.HTTPError as exc:
            return exc.code, exc.read().decode(errors="replace")

    def cleanup(self) -> None:
        self._kill()
        if not self.keep:
            shutil.rmtree(self.iso_root, ignore_errors=True)

    def _kill(self) -> None:
        REGISTRY._kill_group(self.proc.pid)


class EgressWatcher(threading.Thread):
    """Samples `ss -tnp`; records flows touching our ports/pids.

    Any non-loopback peer on those flows is a violation (would catch
    accidental egress to the real upstream if OPENCODE_UPSTREAM_BASE_URL
    ever stopped applying).
    """

    def __init__(self, out_file: Path, ports: set[int], watch_pids: set[int]) -> None:
        super().__init__(daemon=True)
        self.out_file = out_file
        self.port_rx = re.compile(
            "|".join(fr":{re.escape(str(p))}(?!\d)" for p in sorted(ports))
        )
        self.pid_rx = re.compile(
            "|".join(fr"pid={re.escape(str(p))}[,\)]" for p in sorted(watch_pids))
        ) if watch_pids else re.compile(r"a^")  # never-matching fallback
        self.violations: list[str] = []
        self.samples = 0
        self._stop = threading.Event()

    def run(self) -> None:
        while not self._stop.is_set():
            try:
                out = subprocess.run(["ss", "-tnp"],
                                     capture_output=True, text=True,
                                     timeout=5).stdout
            except Exception:  # noqa: BLE001
                out = ""
            for ln in out.splitlines():
                if "((" not in ln:
                    continue  # skip header/empty
                if not (self.port_rx.search(ln) or self.pid_rx.search(ln)):
                    continue
                with open(self.out_file, "a") as fh:
                    fh.write(ln.strip() + "\n")
                for addr, port_s in re.findall(
                        r"(\d+\.\d+\.\d+\.\d+):(\d+)", ln):
                    if not addr.startswith("127.0.0."):
                        self.violations.append(
                            f"non-loopback peer {addr}:{port_s}: {ln.strip()}")
            self.samples += 1
            self._stop.wait(1.0)

    def stop(self) -> None:
        self._stop.set()
        self.join(timeout=5)

    @property
    def sampled(self) -> bool:
        return self.samples > 0


class PtySession:
    """Runs the real `claude` CLI under a PTY, capturing raw terminal bytes."""

    def __init__(self, scenario_dir: Path, claude_bin: str, profile_dir: Path,
                 cols: int = 140, rows: int = 40,
                 workdir: Path | None = None) -> None:
        import fcntl
        import pty
        import struct
        import termios

        self._fcntl, self._struct, self._termios = fcntl, struct, termios
        self.raw_path = scenario_dir / "terminal.raw"
        self.events_path = scenario_dir / "events.jsonl"
        self.raw_f = open(self.raw_path, "wb")
        self.buf = b""
        self.vis = ""                      # ANSI-stripped, CR->LF normalized
        self.lock = threading.Condition()
        self.last_activity = time.monotonic()
        self._dialog_cooldown = 0.0
        self._answered_once: set[str] = set()
        self._pending_vis = ""          # new stripped text not yet scanned
        self.glyph_seen = False         # input prompt glyph rendered
        self.last_blocker_at = time.monotonic()
        self.exit_status: int | None = None

        env = dict(os.environ)
        # Scrub ambient CLAUDE_*/ANTHROPIC_* (this harness itself often runs
        # inside a Claude Code session) so only profile settings.json and the
        # explicit vars below shape the child CLI.
        for k in list(env):
            if k.startswith(("CLAUDE_", "ANTHROPIC_")):
                del env[k]
        env.update({
            "TERM": "xterm-256color",
            "CLAUDE_CONFIG_DIR": str(profile_dir),
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1",
            "DISABLE_TELEMETRY": "1",
            "DISABLE_ERROR_REPORTING": "1",
            "DISABLE_AUTOUPDATER": "1",
        })
        # Load ONLY user-level settings (= CLAUDE_CONFIG_DIR/settings.json);
        # project/managed settings must never reach the test client.
        argv = [claude_bin, "--setting-sources", "user"]
        self.pid, master = pty.fork()
        if self.pid == 0:
            try:
                # Isolated cwd OUTSIDE the repo: the CLI discovers project
                # settings by walking parents up to the git root, and this
                # repo exports its own ANTHROPIC_API_KEY/BASE_URL via
                # .claude/settings*.json which would conflict with ours.
                os.chdir(str(workdir) if workdir else "/tmp")
                os.execvpe(claude_bin, argv, env)
            finally:
                os._exit(127)
        self.master = master
        self._fcntl.ioctl(master, self._termios.TIOCSWINSZ,
                          self._struct.pack("HHHH", rows, cols, 0, 0))
        REGISTRY.add_pid(self.pid)
        threading.Thread(target=self._read_loop, daemon=True).start()

    # -- reader ------------------------------------------------------
    def _read_loop(self) -> None:
        while True:
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                chunk = b""
            if not chunk:
                with self.lock:
                    self.lock.notify_all()
                return
            with self.lock:
                self.buf += chunk
                stripped = strip_ansi(chunk).replace("\r", "\n")
                self.vis = (self.vis + stripped)[-32768:]
                self._pending_vis += stripped
                sq = squash(stripped)
                now = time.monotonic()
                if "❯" in stripped:
                    self.glyph_seen = True
                if any(squash(b) in sq for b in READY_BLOCKERS):
                    self.last_blocker_at = now
                self.last_activity = now
                self.lock.notify_all()
            self.raw_f.write(chunk)
            self.raw_f.flush()
            self._auto_answer()

    def _auto_answer(self) -> None:
        now = time.monotonic()
        if now < self._dialog_cooldown:
            return
        with self.lock:
            fresh, self._pending_vis = self._pending_vis, ""
        if not fresh:
            return
        fresh_sq = squash(fresh)
        for needle, answer in DIALOG_ANSWERS:
            if squash(needle) in fresh_sq and not (
                    needle in ONBOARDING_NEEDLES
                    and needle in self._answered_once):
                try:
                    os.write(self.master, answer)
                except OSError:
                    return
                if needle in ONBOARDING_NEEDLES:
                    self._answered_once.add(needle)
                self.log_event("dialog_answered", needle)
                self._dialog_cooldown = time.monotonic() + 1.5
                return

    # -- helpers -----------------------------------------------------
    def log_event(self, kind: str, detail: str = "") -> None:
        with open(self.events_path, "a") as fh:
            fh.write(json.dumps({"t": round(time.time(), 3),
                                 "monotonic": round(time.monotonic(), 3),
                                 "kind": kind, "detail": detail}) + "\n")

    def send(self, data: bytes) -> None:
        os.write(self.master, data)
        time.sleep(0.12)

    def wait_for(self, pattern: str, timeout: float) -> bool:
        rx = re.compile(pattern)
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            with self.lock:
                if rx.search(self.vis):
                    return True
                self.lock.wait(0.25)
        return False

    def raw_len(self) -> int:
        with self.lock:
            return len(self.buf)

    def vis_window(self, start: int) -> str:
        """Stripped-visible text produced after raw offset `start`."""
        with self.lock:
            return strip_ansi(self.buf[start:]).replace("\r", "\n")

    def wait_ready(self, timeout: float = 120.0) -> bool:
        """Ready once the input glyph has rendered and no modal-dialog
        marker appeared in FRESHLY-rendered text for the last 6s.

        Judged on incremental text only: this TUI's entire session output
        can fit in a few KB, so any accumulated-tail window keeps matching
        long-dismissed dialog wording forever. Byte-idleness is likewise
        unusable — the banner re-renders indefinitely when terminal
        capability queries go unanswered on a bare PTY."""
        start = time.monotonic()
        end = start + timeout
        while time.monotonic() < end:
            with self.lock:
                glyph = self.glyph_seen
                quiet_for = time.monotonic() - self.last_blocker_at
            if glyph and quiet_for > 6.0 and time.monotonic() - start > 5.0:
                self.log_event("repl_ready")
                return True
            time.sleep(0.25)
        self.log_event("repl_never_ready")
        return False

    def send_prompt(self, text: str) -> None:
        self.log_event("prompt_sent", text)
        for attempt in (1, 2):
            mark = self.raw_len()
            self.send(text.encode() + b"\r")
            # Verify the input actually landed in the REPL (a dialog may
            # have swallowed the keystrokes); resend once if not echoed.
            echo_sq = squash(text)
            end = time.monotonic() + 8
            while time.monotonic() < end:
                if echo_sq in squash(self.vis_window(mark)):
                    return
                time.sleep(0.3)
            if attempt == 1:
                self.log_event("prompt_resent", text)
        self.log_event("prompt_echo_missing", text)

    def interrupt(self) -> None:
        self.log_event("ctrl_c")
        os.write(self.master, b"\x03")
        time.sleep(0.5)
        os.write(self.master, b"\x03")   # double-tap, matches prior drivers
        self.log_event("ctrl_c_sent")

    def finish(self, timeout: float = 10.0) -> int:
        """/exit politely, then SIGTERM the group, then SIGKILL."""
        self.log_event("exit_requested")
        try:
            self.send(b"/exit\r")
        except OSError:
            pass
        end = time.monotonic() + timeout
        while time.monotonic() < end and self.exit_status is None:
            try:
                pid, status = os.waitpid(self.pid, os.WNOHANG)
            except ChildProcessError:
                self.exit_status = -1
                break
            if pid == self.pid:
                self.exit_status = status
                break
            time.sleep(0.2)
        if self.exit_status is None:
            REGISTRY._kill_group(self.pid)
            self.exit_status = -2
        try:
            self.raw_f.flush()
        except OSError:
            pass
        return self.exit_status


# ---------------------------------------------------------------------------
# Mechanical checks
# ---------------------------------------------------------------------------

def check_window_clean(window_vis: str) -> list[str]:
    """Spinner residue / leaked protocol lines in a quiet-window slice."""
    fails = []
    if SPINNER_RE.search(window_vis):
        fails.append("spinner glyphs present in post-event window")
    if RAW_SSE_RE.search(window_vis):
        fails.append("raw SSE event/data line leaked into terminal")
    return fails


def scan_suspicious(vis_text: str) -> list[str]:
    fails = []
    if RAW_SSE_RE.search(vis_text):
        fails.append("raw SSE event/data line visible")
    for pat, label in [
        (r'\{"type"\s*:\s*"(message|content_block)', "raw Anthropic JSON event"),
        (r'"chat\.completion\.chunk"', "raw OpenAI chunk JSON"),
        (r"\[DONE\]", "raw [DONE] sentinel"),
    ]:
        if re.search(pat, vis_text):
            fails.append(label)
    return fails


def assert_no_stuck_redraw(vis_text: str) -> list[str]:
    """No 3+ consecutive identical non-empty lines (stuck redraw loops)."""
    lines = [ln.strip() for ln in vis_text.splitlines()]
    run = 1
    for prev, cur in zip(lines, lines[1:]):
        run = run + 1 if cur and cur == prev else 1
        if run >= 3:
            return [f"line repeated {run}x consecutively: {cur[:80]!r}"]
    return []


def rendered_count(window_vis: str, token: str) -> int:
    """Count lines that consist (modulo TUI chrome) of exactly `token`."""
    rx = re.compile(rf"(?m)^[\s│▌▐●>❯*·✦⏿-]{{0,6}}{re.escape(token)}"
                    rf"[\s│▌▐<·—-]{{0,6}}$")
    return len(rx.findall(window_vis))


# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------

def wait_done(stub: StubUpstream, target: int, timeout: float) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if stub.done_count() >= target:
            time.sleep(1.0)   # let the TUI render the finished turn
            return True
        time.sleep(0.25)
    return False


def scenario_two_consecutive(ctx: ScenarioContext) -> tuple[bool, list[str]]:
    s, stub = ctx.session, ctx.stub
    base_done = stub.done_count()
    ctx.mark_window()
    s.send_prompt("Reply with exactly one word: OK")
    if not wait_done(stub, base_done + 1, 60):
        return False, ["first turn never completed upstream"]
    s.send_prompt("Reply with exactly one word: OK again")
    if not wait_done(stub, base_done + 2, 60):
        return False, ["second turn never completed upstream"]
    ok_n = rendered_count(s.vis_window(ctx.window_start), "OK")
    reqs = stub.req_count() - ctx.base_reqs
    fails = []
    if ok_n < 2:
        fails.append(f"expected >=2 rendered OK replies, saw {ok_n}")
    if reqs < 2:
        fails.append(f"expected >=2 upstream requests, saw {reqs}")
    return not fails, fails


def scenario_agent_tool_call(ctx: ScenarioContext) -> tuple[bool, list[str]]:
    s, stub = ctx.session, ctx.stub
    base_done = stub.done_count()
    ctx.mark_window()
    s.send_prompt("scenario agent: use your agent/subagent tool to list files "
                  "in this directory, then report what it returned")
    if not s.wait_for(r"TOOL_RESULT_ACCEPTED", 180):
        return False, ["agent tool-call continuation never rendered"]
    reqs = stub.req_count() - ctx.base_reqs
    done = stub.done_count() - base_done
    fails = []
    if reqs < 2:
        fails.append(f"expected >=2 upstream requests (call + continuation), saw {reqs}")
    if done < 2:
        fails.append(f"expected >=2 completed streams, saw {done}")
    return not fails, fails


def scenario_streaming_tool_call(ctx: ScenarioContext) -> tuple[bool, list[str]]:
    s, stub = ctx.session, ctx.stub
    base_done = stub.done_count()
    ctx.mark_window()
    s.send_prompt("scenario tool: run the Bash command echo ok and tell me "
                  "its output")
    if not s.wait_for(r"TOOL_RESULT_ACCEPTED", 120):
        return False, ["streaming Bash tool-call continuation never rendered"]
    win = s.vis_window(ctx.window_start)
    reqs = stub.req_count() - ctx.base_reqs
    fails = []
    if reqs < 2:
        fails.append(f"expected >=2 upstream requests, saw {reqs}")
    if not re.search(r"(?m)^\s*(?:.{0,6})ok\s*$", win.replace("\r", "")):
        fails.append("echoed 'ok' output never rendered near the Bash call")
    return not fails, fails


def scenario_shell_command(ctx: ScenarioContext) -> tuple[bool, list[str]]:
    """TUI bash-mode `!`: the CLI executes the command LOCALLY and renders
    its output (v2.1.x then also sends ONE model follow-up summarizing the
    result — observed against the reference deployment too — so upstream
    traffic is allowed here, it just must complete cleanly).
    Plus a direct HTTP check of the BRIDGE-side `!` interception."""
    s, stub, bridge = ctx.session, ctx.stub, ctx.bridge
    nonce = ctx.nonce
    marker = f"PTY_SHELL_OK_{nonce}"
    ctx.mark_window()
    base_done = stub.done_count()
    s.send_prompt(f"!printf {marker}")
    if not s.wait_for(re.escape(marker), 25):
        return False, ["bash-mode output marker never rendered"]
    time.sleep(3.0)
    reqs = stub.req_count() - ctx.base_reqs
    if stub.done_count() < base_done + reqs:
        return False, [f"{reqs} model follow-up request(s) after the `!` "
                       "command did not all complete"]

    status, body = bridge.post_messages({
        "model": "claude-sonnet-4-6",
        "max_tokens": 64,
        "stream": False,
        "messages": [{"role": "user",
                      "content": f"!printf PTY_BRIDGE_SHELL_OK_{nonce}"}],
    })
    if status != 200:
        return False, [f"bridge-side `!` failed: HTTP {status}: {body[:200]}"]
    if f"PTY_BRIDGE_SHELL_OK_{nonce}" not in body:
        return False, [f"bridge did not execute `!` locally; body: {body[:300]}"]
    return True, []


def scenario_ctrlc_midstream(ctx: ScenarioContext) -> tuple[bool, list[str]]:
    s = ctx.session
    nonce = ctx.nonce
    ctx.mark_window()
    s.send_prompt(f"scenario slow nonce-{nonce}: write me a very long story")
    if not s.wait_for(r"esc to interrupt|slow thinking|middle", 40):
        return False, ["stream never visibly started before Ctrl+C"]
    time.sleep(1.0)
    mark = s.raw_len()
    s.interrupt()
    s.wait_for(r"[Ii]nterrupted", 10)     # best-effort visual ack
    time.sleep(1.5)
    fails = check_window_clean(s.vis_window(mark))
    # The REPL must survive: next prompt completes a fresh upstream turn.
    base_done = ctx.stub.done_count()
    s.send_prompt("Reply with exactly one word: OK")
    if not wait_done(ctx.stub, base_done + 1, 90):
        fails.append("post-Ctrl+C prompt never completed (REPL dead?)")
    elif not s.wait_for(r"(?m)^[\s│▌▐●>❯*·]{0,6}OK[\s│▌▐<·—-]{0,6}$", 15):
        fails.append("post-Ctrl+C response never rendered")
    return not fails, fails


def scenario_upstream_error_retry(ctx: ScenarioContext) -> tuple[bool, list[str]]:
    """Stub 500s the FIRST attempt; the CLI must recover through its own
    retry (the bridge fails fast on ProviderServer errors and never retries
    a client turn internally, so a 2nd upstream request can only come from
    the client). Observed v2.1.246 recovery: a silent NON-STREAMING retry
    (no error banner rendered, reply seamless) — so completion is proven by
    done-marker OR request-delta, never by done-marker alone: the stub's
    sync path returns before writing a done-marker."""
    s, stub = ctx.session, ctx.stub
    nonce = ctx.nonce
    prompt = f"scenario error nonce-{nonce}: reply with exactly one word: OK"
    ctx.mark_window()
    base_reqs, base_done = ctx.base_reqs, stub.done_count()
    s.send_prompt(prompt)
    end = time.monotonic() + 120
    completed = False
    while time.monotonic() < end:
        if (stub.done_count() >= base_done + 1        # streaming retry finished
                or stub.req_count() >= base_reqs + 2):  # retry attempt arrived
            completed = True
            break
        time.sleep(0.25)
    if not completed:
        return False, ["turn never completed even after retries"]
    time.sleep(1.0)
    fails = []
    reqs = stub.req_count() - base_reqs
    if reqs < 2:
        fails.append(f"expected >=2 upstream attempts (500 then success), saw {reqs}")
    # Nonce-level req-log matching is impossible: the stub truncates
    # last_user to 60 chars and the nonce sits behind a long
    # <system-reminder> prefix, so it never reaches stub.req.log (doc
    # limitation #3). Retry proof is structural instead: the stub serves
    # exactly one 500 per distinct "scenario error" prompt and succeeds on
    # every identical repeat, hence >=2 requests == >=1 client retry.
    if not s.wait_for(r"(?m)^[\s│▌▐●>❯*·]{0,6}OK[\s│▌▐<·—-]{0,6}$", 15):
        fails.append("final successful response never rendered")
    # Error recovery must stay protocol-clean: no leaked error bodies,
    # raw SSE, or chunk JSON in anything the terminal rendered this turn.
    fails += scan_suspicious(s.vis_window(ctx.window_start))
    return not fails, fails


def scenario_ten_turns(ctx: ScenarioContext) -> tuple[bool, list[str]]:
    s, stub = ctx.session, ctx.stub
    base_done = stub.done_count()
    ctx.mark_window()
    for i in range(1, 11):
        s.send_prompt(f"Turn {i} of 10: reply with exactly one word: OK")
        if not wait_done(stub, base_done + i, 60):
            return False, [f"turn {i} never completed upstream"]
        if not s.wait_for(r"OK", 15):
            return False, [f"turn {i} response never rendered"]
    time.sleep(3.0)                       # idle window: leftovers show here
    idle_mark = max(s.raw_len() - 4096, 0)
    fails = check_window_clean(s.vis_window(idle_mark))
    whole = s.vis_window(ctx.window_start)
    fails += scan_suspicious(whole)
    fails += assert_no_stuck_redraw(whole)
    reqs = stub.req_count() - ctx.base_reqs
    if reqs < 10:
        fails.append(f"expected >=10 upstream requests, saw {reqs}")
    return not fails, fails


def scenario_midstream_error_terminates_cleanly(
        ctx: ScenarioContext) -> tuple[bool, list[str]]:
    """Stub streams one `partial` content chunk, then a raw in-band OpenAI
    error data line (`data: {"error": {...500...}}`) and stops — no [DONE],
    no finish_reason. Deploy-gate invariant #6 (mid-stream half): the bridge
    must surface this as ONE terminal Anthropic `error` event that ENDS the
    stream — no message_delta/message_stop after it.

    Mechanical proof shape (no pixel comparison available):
      * EXACTLY ONE streaming upstream request delivers the midfail script
        (done-marker +1 proves its scripted bytes were fully flushed). A
        SECOND streaming attempt for this turn — client re-drive or bridge
        internal replay — is a hard FAIL: a terminal error event ends the
        conversation turn;
      * bridge log contains EXACTLY ONE INFO marker "mid-stream upstream
        error event; stream ended at the error" (emitted only on the
        error_terminated break BEFORE message_delta/message_stop
        finalization) and NO wrong-path "Stream failed after visible
        output";
      * scan_suspicious over the whole window: no raw SSE / chunk JSON /
        [DONE] leaked to the terminal;
      * REPL survives: the follow-up turn completes upstream AND renders -
        which also mechanically proves the failed turn ENDED (the TUI cannot
        complete a new upstream turn while the previous one is stuck).

    Pinned client behavior (observed 2026-08-26, v2.1.246, NOT gated — see
    scripts/pty_matrix.md limitation on in-band-error recovery): after the
    well-formed terminal error event the CLI recovers through the SAME
    silent NON-streaming fallback as S6 — one extra `stream=False` request
    the stub answers with plain JSON OK. It is recorded in events.jsonl
    (client_nonstreaming_fallback) but is client retry policy, outside
    invariant #6's termination-shape claims; a re-driven STREAMING request
    WOULD gate-fail above."""
    s, stub, bridge = ctx.session, ctx.stub, ctx.bridge
    nonce = ctx.nonce

    def req_modes() -> tuple[int, int]:
        """(stream=True, stream=False) counts over THIS scenario's requests."""
        try:
            lines = Path(stub.req_log).read_text(errors="replace") \
                .splitlines()[ctx.base_reqs:]
        except OSError:
            return -1, -1
        return (sum("stream=True" in ln for ln in lines),
                sum("stream=False" in ln for ln in lines))

    base_done = stub.done_count()
    ctx.mark_window()
    s.send_prompt(f"scenario midfail nonce-{nonce}: finish the sentence")
    if not wait_done(stub, base_done + 1, 60):
        return False, ["midfail stream never completed its scripted bytes "
                       "upstream"]
    s.log_event("midfail_upstream_delivered")

    # Observation window: catch an immediate re-drive of the failed stream.
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        if stub.req_count() > ctx.base_reqs + 1:
            break
        time.sleep(0.5)

    streams, fallbacks = req_modes()
    if fallbacks > 0:
        s.log_event("client_nonstreaming_fallback",
                    f"{fallbacks} stream=False recovery request(s)")

    fails: list[str] = []
    if streams != 1:
        fails.append(
            f"FINDING failed stream was re-driven: expected exactly 1 "
            f"stream=True request for the midfail turn, saw {streams} "
            f"(stub.req.log + terminal.raw hold the evidence)")

    # Bridge-side termination-shape proof from its own logs (RUST_LOG=info).
    try:
        blog = Path(bridge.logs["stdout"]).read_text(errors="replace") \
            + Path(bridge.logs["stderr"]).read_text(errors="replace")
    except OSError:
        blog = ""
        fails.append("bridge logs unreadable for termination-shape check")
    n_marker = blog.count("mid-stream upstream error event")
    if n_marker != 1:
        fails.append(f"bridge 'mid-stream upstream error event' marker seen "
                     f"{n_marker}x, expected exactly 1")
    if "Stream failed after visible output" in blog:
        fails.append("bridge took the wrong path: 'Stream failed after "
                     "visible output' logged instead of clean error "
                     "termination")

    # Nothing protocol-shaped may reach the terminal this scenario.
    whole = s.vis_window(ctx.window_start)
    fails += scan_suspicious(whole)

    # Best-effort visual ack of the failure (NOT gated on wording).
    if s.wait_for(r"[Aa][Pp][Ii]\s*[Ee]rror", 10):
        s.log_event("cli_rendered_api_error_banner")

    # Turn ENDED + REPL survives: a fresh follow-up turn must round-trip.
    fb_base = stub.done_count()
    s.send_prompt("Reply with exactly one word: OK")
    if not wait_done(stub, fb_base + 1, 90):
        fails.append("follow-up turn never completed (turn hung or REPL dead?)")
    elif not s.wait_for(r"(?m)^[\s│▌▐●>❯*·]{0,6}OK[\s│▌▐<·—-]{0,6}$", 15):
        fails.append("follow-up response never rendered")

    # Late re-drive check: across the WHOLE scenario only the midfail turn +
    # the follow-up may ever arrive as streaming requests.
    streams_final, _ = req_modes()
    if streams_final != 2:
        fails.append(
            f"FINDING unexpected streaming traffic: expected exactly 2 "
            f"stream=True requests scenario-wide (midfail + follow-up), "
            f"saw {streams_final}")
    return not fails, fails


SCENARIOS = {
    "1": ("two_consecutive", scenario_two_consecutive),
    "2": ("agent_tool_call", scenario_agent_tool_call),
    "3": ("streaming_tool_call", scenario_streaming_tool_call),
    "4": ("shell_command", scenario_shell_command),
    "5": ("ctrlc_midstream", scenario_ctrlc_midstream),
    "6": ("upstream_error_retry", scenario_upstream_error_retry),
    "7": ("ten_turns", scenario_ten_turns),
    "8": ("midstream_error_terminates_cleanly",
          scenario_midstream_error_terminates_cleanly),
}

SCENARIO_BUDGET_S = {
    "1": 150, "2": 300, "3": 200, "4": 100, "5": 220, "6": 220, "7": 420,
    "8": 220,
}


class ScenarioContext:
    def __init__(self, num: str, out_dir: Path, bridge: BridgeInstance,
                 stub: StubUpstream, claude_bin: str) -> None:
        self.num = num
        self.out_dir = out_dir
        self.bridge, self.stub = bridge, stub
        self.claude_bin = claude_bin
        self.nonce = f"{int(time.time()) % 100000}{num}"
        self.session: PtySession | None = None
        self.watcher: EgressWatcher | None = None
        self.base_reqs = 0
        self.window_start = 0

    def mark_window(self) -> None:
        self.base_reqs = self.stub.req_count()
        if self.session:
            self.window_start = self.session.raw_len()


def run_scenario(num: str, args: argparse.Namespace) -> dict:
    name, fn = SCENARIOS[num]
    out_dir = args.out / f"{time.strftime('%H%M%S')}-s{num}-{name}"
    out_dir.mkdir(parents=True, exist_ok=True)
    result: dict = {"scenario": f"S{num} {name}", "pass": False, "fails": []}

    stub = bridge = ctx = None
    started = time.monotonic()
    try:
        stub = StubUpstream(out_dir, args.stub)
        bridge = BridgeInstance(out_dir, args.bridge_bin, stub.base_url,
                                args.shell_policy)
        profile = out_dir / "claude-profile"
        profile.mkdir()
        (profile / "settings.json").write_text(json.dumps({
            "model": args.model,
            "env": {
                "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{bridge.port}",
                # AUTH_TOKEN (Bearer) instead of API_KEY: the CLI's
                # "custom API key" dialog only triggers on ANTHROPIC_API_KEY,
                # and its default answer leaves the CLI logged out.
                "ANTHROPIC_AUTH_TOKEN": "opencode-bridge",
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000",
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "128000",
                "CLAUDE_CODE_DISABLE_1M_CONTEXT": "1",
            },
        }, indent=2) + "\n")
        # Pre-provision onboarding state so the TUI lands straight in the
        # REPL (theme + API-key dialogs skipped; folder-trust auto-answered).
        (profile / ".claude.json").write_text(json.dumps({
            "theme": "dark",
            "hasCompletedOnboarding": True,
        }) + "\n")

        ctx = ScenarioContext(num, out_dir, bridge, stub, args.claude_bin)
        # CLI working dir must live OUTSIDE the repo (project-settings walk).
        workdir = Path(tempfile.mkdtemp(prefix=f"oc2api-pty-work-s{num}-"))
        REGISTRY.temp_dirs.append(workdir)
        ctx.watcher = EgressWatcher(out_dir / "egress.log",
                                    {bridge.port, stub.port},
                                    {bridge.proc.pid, stub.proc.pid})
        ctx.watcher.start()
        ctx.session = PtySession(out_dir, args.claude_bin, profile,
                                 workdir=workdir)
        if not ctx.session.wait_ready():
            raise RuntimeError(
                "REPL never became ready (onboarding stuck?) — see "
                f"{out_dir / 'terminal.raw'}")

        ok, fails = fn(ctx)
        egress_fails = list(ctx.watcher.violations)
        if not ctx.watcher.sampled:
            egress_fails.append("egress watcher produced no samples")
        result["fails"] = list(fails) + egress_fails
        result["pass"] = ok and not egress_fails
    except Exception as exc:  # noqa: BLE001
        result["fails"] = ([f"harness error: {exc!r}"]
                           + traceback.format_exc(limit=8).splitlines())
        result["pass"] = False
    finally:
        if ctx and ctx.watcher:
            try:
                ctx.watcher.stop()
            except Exception:  # noqa: BLE001
                pass
        if ctx and ctx.session:
            try:
                ctx.session.finish()
            except Exception:  # noqa: BLE001
                REGISTRY._kill_group(ctx.session.pid)
        if bridge:
            bridge.cleanup()
        if stub:
            REGISTRY._kill_group(stub.proc.pid)
        result["seconds"] = round(time.monotonic() - started, 1)
        result["artifacts"] = str(out_dir)

    (out_dir / "RESULT.json").write_text(json.dumps(result, indent=2) + "\n")
    status = "PASS" if result["pass"] else "FAIL"
    print(f"[S{num}] {name:<22} {status}  ({result['seconds']}s)", flush=True)
    for f in result["fails"][:8]:
        print(f"       - {f}", flush=True)
    return result


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(
        description="Real-CLI PTY verification matrix (deploy gate harness)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("list", help="list scenarios and exit")
    p_run = sub.add_parser("run", help="run scenarios (fresh bridge pair each)")
    p_smoke = sub.add_parser("smoke", help="cheap gate: scenarios 1 + 4")
    for p in (p_run, p_smoke):
        p.add_argument("--scenarios", default=None,
                       help="comma list like 1,4 or 'all' (run only)")
        p.add_argument("--out", type=Path,
                       default=ROOT / "artifacts" / "pty-matrix")
        p.add_argument("--bridge-bin", type=Path, default=DEFAULT_BRIDGE_BIN)
        p.add_argument("--stub", type=Path, default=None,
                       help="OpenAI SSE stub path (default: auto-discover)")
        p.add_argument("--claude-bin", default=shutil.which("claude") or "claude")
        p.add_argument("--model", default="claude-sonnet-4-6")
        p.add_argument("--shell-policy", default="unrestricted",
                       choices=["unrestricted", "allowlist", "disabled"])
    ns = ap.parse_args()
    if ns.cmd == "list":
        for k, (name, _) in SCENARIOS.items():
            print(f"S{k}: {name:<22} budget<= {SCENARIO_BUDGET_S[k]}s")
        return 0

    if ns.cmd == "smoke":
        nums = ["1", "4"]
    elif ns.scenarios == "all":
        nums = list(SCENARIOS)
    else:
        nums = [t.strip() for t in (ns.scenarios or "").split(",") if t.strip()]
    bad = [n for n in nums if n not in SCENARIOS]
    if bad or not nums:
        print(f"unknown/empty scenarios: {bad}", file=sys.stderr)
        return 2

    stub_path = ns.stub or next((c for c in STUB_CANDIDATES if c.exists()), None)
    if stub_path is None:
        print("no stub found; looked for:\n  "
              + "\n  ".join(map(str, STUB_CANDIDATES)), file=sys.stderr)
        return 2
    ns.stub = stub_path.resolve()
    ns.out.mkdir(parents=True, exist_ok=True)

    results = [run_scenario(n, ns) for n in nums]
    summary = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "claude_version": subprocess.run([ns.claude_bin, "--version"],
                                         capture_output=True,
                                         text=True).stdout.strip(),
        "bridge_binary": str(ns.bridge_bin),
        "stub": str(ns.stub),
        "summary": {"total": len(results),
                    "passed": sum(r["pass"] for r in results),
                    "failed": sum(not r["pass"] for r in results)},
        "scenarios": results,
    }
    summary_path = ns.out / f"{time.strftime('%Y%m%d-%H%M%S')}-summary.json"
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    s = summary["summary"]
    print(f"\nSummary: {s['passed']}/{s['total']} PASS -> {summary_path}",
          flush=True)
    return 0 if s["failed"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
